//! Live integration test for the multiplexed-connection pattern the app's
//! connection actor relies on: hold `Arc<RusshSession>`, run `event_shell`
//! (now `&self`) while issuing SFTP on the *same* connection concurrently.
//!
//! Gated on `OPENTERM_TEST_PASSWORD` so it is skipped in offline CI. Run with:
//!   OPENTERM_TEST_PASSWORD=… cargo test -p openterm-ssh --test live_multiplex -- --nocapture

use std::sync::Arc;
use std::time::Duration;

use openterm_core::HostProfile;
use openterm_ssh::{
    AuthMethod, ConnectOptions, ConnectRoute, HostKeyPolicy, PtyEvent, PtyInput, PtySize,
    RemoteFileKind, RusshBackend, ShellOptions,
};
use tokio::sync::mpsc;

const HOST: &str = "82.157.57.178";
const USER: &str = "ubuntu";

fn route(password: String) -> ConnectRoute {
    let mut profile = HostProfile::new("live", HOST);
    profile.port = 22;
    profile.username = Some(USER.to_string());
    ConnectRoute {
        target: profile,
        target_options: ConnectOptions {
            username: USER.to_string(),
            auth: AuthMethod::Password(password),
            trust_unknown_host_keys: true,
            host_key_policy: HostKeyPolicy::TrustAll,
            timeout: Duration::from_secs(15),
        },
        jump: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_and_sftp_multiplex_on_one_connection() {
    let Ok(password) = std::env::var("OPENTERM_TEST_PASSWORD") else {
        eprintln!("skipping: OPENTERM_TEST_PASSWORD not set");
        return;
    };

    // One connection, shared via Arc — exactly like the app's actor.
    let session = Arc::new(
        RusshBackend
            .connect_with_route(route(password))
            .await
            .expect("connect"),
    );

    // Shell on its own task (event_shell takes &self now).
    let (in_tx, mut in_rx) = mpsc::channel::<PtyInput>(64);
    let (ev_tx, mut ev_rx) = mpsc::channel::<PtyEvent>(256);
    let shell_session = session.clone();
    let shell = tokio::spawn(async move {
        shell_session
            .event_shell(
                ShellOptions {
                    term: "xterm-256color".to_string(),
                    size: PtySize {
                        cols: 100,
                        rows: 30,
                    },
                },
                &mut in_rx,
                ev_tx,
            )
            .await
    });

    // Concurrently list a directory over the SAME connection — this is the
    // multiplexing the old app never did (it redialed per SFTP op).
    let sftp_session = session.clone();
    let listing = tokio::spawn(async move { sftp_session.list_dir(".").await });

    // Type a command into the shell and look for its echo/output.
    let marker = format!("OPENTERM_LIVE_{}", std::process::id());
    in_tx
        .send(PtyInput::Write(format!("echo {marker}\n").into_bytes()))
        .await
        .unwrap();

    let mut shell_saw_marker = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), ev_rx.recv()).await {
            Ok(Some(PtyEvent::Output(bytes))) => {
                if String::from_utf8_lossy(&bytes).contains(&marker) {
                    shell_saw_marker = true;
                    break;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }

    let sftp_result = listing.await.expect("sftp task");
    let entries = sftp_result.expect("sftp list ok");

    // Disconnect via &self (the actor's teardown path).
    session.disconnect().await.expect("disconnect");
    let _ = in_tx; // drop to end shell input
    let _ = shell.await;

    assert!(shell_saw_marker, "shell did not echo marker over PTY");
    assert!(!entries.is_empty(), "sftp listing was empty");
    eprintln!(
        "OK: shell PTY echoed marker AND sftp listed {} entries on one connection",
        entries.len()
    );
}

/// Full SFTP round-trip over the live connection: upload, confirm via listing,
/// download, verify bytes match, then remove. Exercises the exact backend calls
/// the app's connection actor uses for upload/download/delete.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sftp_round_trip_on_live_connection() {
    let Ok(password) = std::env::var("OPENTERM_TEST_PASSWORD") else {
        eprintln!("skipping: OPENTERM_TEST_PASSWORD not set");
        return;
    };

    let session = RusshBackend
        .connect_with_route(route(password))
        .await
        .expect("connect");

    let name = format!("openterm_roundtrip_{}.txt", std::process::id());
    let remote = format!("/tmp/{name}");
    let payload = format!("openterm round-trip {}\n", std::process::id()).into_bytes();

    // Upload.
    session
        .write_file(&remote, payload.clone())
        .await
        .expect("upload");

    // Confirm it appears in the listing.
    let listed = session.list_dir("/tmp").await.expect("list /tmp");
    assert!(
        listed.iter().any(|e| e.name == name),
        "uploaded file not found in remote listing"
    );

    // Download and verify bytes.
    let fetched = session.read_file(&remote).await.expect("download");
    assert_eq!(fetched, payload, "downloaded bytes differ from uploaded");

    // Clean up.
    session
        .remove_path(&remote, RemoteFileKind::File)
        .await
        .expect("remove");
    let after = session.list_dir("/tmp").await.expect("list after remove");
    assert!(
        !after.iter().any(|e| e.name == name),
        "file still present after remove"
    );

    session.disconnect().await.expect("disconnect");
    eprintln!("OK: SFTP upload -> list -> download (verified) -> remove round-trip");
}

/// Streaming upload/download with progress reporting (the path the app's live
/// transfer UI uses). Verifies progress is monotonic and bytes round-trip.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_transfer_reports_progress() {
    let Ok(password) = std::env::var("OPENTERM_TEST_PASSWORD") else {
        eprintln!("skipping: OPENTERM_TEST_PASSWORD not set");
        return;
    };

    let session = RusshBackend
        .connect_with_route(route(password))
        .await
        .expect("connect");

    // Make a ~512 KiB local file so streaming runs several chunks.
    let payload: Vec<u8> = (0..512 * 1024).map(|i| (i % 251) as u8).collect();
    let local_up = std::env::temp_dir().join(format!("openterm_up_{}.bin", std::process::id()));
    let local_down = std::env::temp_dir().join(format!("openterm_down_{}.bin", std::process::id()));
    tokio::fs::write(&local_up, &payload).await.unwrap();
    let remote = format!("/tmp/openterm_stream_{}.bin", std::process::id());

    // Upload with progress.
    let (ptx, mut prx) = tokio::sync::mpsc::channel::<u64>(64);
    let up_session = &session;
    let collect = tokio::spawn(async move {
        let mut samples = Vec::new();
        while let Some(n) = prx.recv().await {
            samples.push(n);
        }
        samples
    });
    let sent = up_session
        .upload_file(&local_up, &remote, ptx, Arc::new(std::sync::atomic::AtomicU8::new(0)))
        .await
        .expect("upload");
    let up_samples = collect.await.unwrap();
    assert_eq!(sent, payload.len() as u64);
    assert!(!up_samples.is_empty(), "no upload progress samples");
    assert!(
        up_samples.windows(2).all(|w| w[1] >= w[0]),
        "upload progress not monotonic"
    );
    assert_eq!(*up_samples.last().unwrap(), payload.len() as u64);

    // Download with progress.
    let (dtx, mut drx) = tokio::sync::mpsc::channel::<u64>(64);
    let collect = tokio::spawn(async move {
        let mut last = 0;
        while let Some(n) = drx.recv().await {
            last = n;
        }
        last
    });
    let got = session
        .download_file(&remote, &local_down, dtx, Arc::new(std::sync::atomic::AtomicU8::new(0)))
        .await
        .expect("download");
    let last_down = collect.await.unwrap();
    assert_eq!(got, payload.len() as u64);
    assert_eq!(last_down, payload.len() as u64);

    let roundtrip = tokio::fs::read(&local_down).await.unwrap();
    assert_eq!(roundtrip, payload, "downloaded bytes differ");

    // Cleanup.
    session
        .remove_path(&remote, RemoteFileKind::File)
        .await
        .expect("remove");
    let _ = tokio::fs::remove_file(&local_up).await;
    let _ = tokio::fs::remove_file(&local_down).await;
    session.disconnect().await.expect("disconnect");
    eprintln!(
        "OK: streaming upload+download of {} KiB with monotonic progress",
        payload.len() / 1024
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canonicalize_resolves_relative_path() {
    let Ok(password) = std::env::var("OPENTERM_TEST_PASSWORD") else {
        eprintln!("skipping: OPENTERM_TEST_PASSWORD not set");
        return;
    };
    let session = RusshBackend
        .connect_with_route(route(password))
        .await
        .expect("connect");
    let abs = session.canonicalize(".").await.expect("canonicalize");
    assert!(abs.starts_with('/'), "expected absolute path, got {abs:?}");
    assert!(abs.contains("ubuntu"), "expected home dir, got {abs:?}");
    session.disconnect().await.expect("disconnect");
    eprintln!("OK: '.' canonicalized to {abs}");
}

/// Regression: an idle session used to die after ~`timeout` seconds because
/// `inactivity_timeout` was wired to the connect timeout. Now the session must
/// survive well past that window (kept alive by keepalives) and still be usable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_session_survives_past_connect_timeout() {
    let Ok(password) = std::env::var("OPENTERM_TEST_PASSWORD") else {
        eprintln!("skipping: OPENTERM_TEST_PASSWORD not set");
        return;
    };
    let session = RusshBackend
        .connect_with_route(route(password))
        .await
        .expect("connect");

    // Sit idle for longer than the old 15s inactivity window (no traffic at all).
    eprintln!("idle for 25s to prove the session is not dropped...");
    tokio::time::sleep(Duration::from_secs(25)).await;

    // Still usable: an SFTP op must succeed after the idle period.
    let abs = session
        .canonicalize(".")
        .await
        .expect("session should still be alive after idle");
    assert!(abs.starts_with('/'), "expected absolute path, got {abs:?}");
    session.disconnect().await.expect("disconnect");
    eprintln!("OK: session alive after 25s idle, canonicalize -> {abs}");
}

/// The resource monitor runs one combined command per poll over the live
/// connection. Verify `exec_capture` returns the `/proc` sections we parse.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_capture_returns_proc_sections() {
    let Ok(password) = std::env::var("OPENTERM_TEST_PASSWORD") else {
        eprintln!("skipping: OPENTERM_TEST_PASSWORD not set");
        return;
    };
    let session = RusshBackend
        .connect_with_route(route(password))
        .await
        .expect("connect");
    // Mirror the app's combined sample command (a representative subset).
    let cmd = "echo @@CPU@@; grep '^cpu ' /proc/stat; echo @@MEM@@; \
               grep MemTotal /proc/meminfo; echo @@END@@";
    let out = session.exec_capture(cmd).await.expect("exec_capture");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("@@CPU@@"), "missing CPU marker: {text}");
    assert!(text.contains("cpu "), "missing /proc/stat cpu line: {text}");
    assert!(text.contains("MemTotal"), "missing MemTotal: {text}");
    session.disconnect().await.expect("disconnect");
    eprintln!(
        "OK: exec_capture returned {} bytes of /proc data",
        out.stdout.len()
    );
}

/// Print the full monitor sample so we can confirm the real server's output
/// format matches the parser's assumptions. Eyeball-only; asserts key markers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_sample_command_format() {
    let Ok(password) = std::env::var("OPENTERM_TEST_PASSWORD") else {
        eprintln!("skipping: OPENTERM_TEST_PASSWORD not set");
        return;
    };
    let session = RusshBackend
        .connect_with_route(route(password))
        .await
        .expect("connect");
    // Keep in sync with crate::metrics::SAMPLE_COMMAND in openterm-app.
    let cmd = "\
echo @@CPU@@; grep '^cpu' /proc/stat 2>/dev/null; \
echo @@MEM@@; cat /proc/meminfo 2>/dev/null; \
echo @@LOAD@@; cat /proc/loadavg 2>/dev/null; \
echo @@UP@@; cat /proc/uptime 2>/dev/null; \
echo @@DISK@@; cat /proc/diskstats 2>/dev/null; \
echo @@NET@@; cat /proc/net/dev 2>/dev/null; \
echo @@DF@@; df -kP / 2>/dev/null; \
echo @@END@@";
    let out = session.exec_capture(cmd).await.expect("exec_capture");
    let text = String::from_utf8_lossy(&out.stdout);
    for marker in ["@@CPU@@", "@@MEM@@", "@@LOAD@@", "@@DF@@", "@@END@@"] {
        assert!(text.contains(marker), "missing {marker}");
    }
    eprintln!("---- real sample ----\n{text}\n---- end ----");
    session.disconnect().await.expect("disconnect");
}

/// The monitor's process drill-down runs `ps` over the live connection. Verify
/// the real server returns a parseable table (header + at least the PID 1 row).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ps_process_list_format() {
    let Ok(password) = std::env::var("OPENTERM_TEST_PASSWORD") else {
        eprintln!("skipping: OPENTERM_TEST_PASSWORD not set");
        return;
    };
    let session = RusshBackend
        .connect_with_route(route(password))
        .await
        .expect("connect");
    // Mirror crate::metrics::PROCESS_COMMAND in openterm-app.
    let cmd = "echo @@PS@@; ps -eo pid,user:20,pcpu,pmem,rss,comm 2>/dev/null; echo @@END@@";
    let out = session.exec_capture(cmd).await.expect("exec_capture");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("@@PS@@"), "missing PS marker");
    assert!(text.contains("PID"), "missing ps header: {text}");
    // At least a handful of process rows on any real server.
    let rows = text
        .lines()
        .filter(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit()))
        .count();
    assert!(rows >= 3, "expected several process rows, got {rows}");
    eprintln!("---- ps (first 12 lines) ----");
    for l in text.lines().take(12) {
        eprintln!("{l}");
    }
    session.disconnect().await.expect("disconnect");
    eprintln!("OK: ps returned {rows} process rows");
}

/// Regression: deleting a *non-empty* remote folder used to fail silently
/// (SFTP rmdir only removes empty dirs). Verify recursive removal: build a
/// dir with a file and a nested subdir, then remove_path it as a Directory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recursive_remote_dir_delete() {
    let Ok(password) = std::env::var("OPENTERM_TEST_PASSWORD") else {
        eprintln!("skipping: OPENTERM_TEST_PASSWORD not set");
        return;
    };
    let session = RusshBackend
        .connect_with_route(route(password))
        .await
        .expect("connect");

    let base = format!("/tmp/openterm_rmtest_{}", std::process::id());
    let sub = format!("{base}/nested");
    session.create_dir(&base).await.expect("mkdir base");
    session.create_dir(&sub).await.expect("mkdir nested");

    // Drop a file into each level so the tree is non-empty.
    let payload = b"openterm recursive delete test\n";
    for path in [format!("{base}/a.txt"), format!("{sub}/b.txt")] {
        let tmp = std::env::temp_dir().join("openterm_rm_src.txt");
        tokio::fs::write(&tmp, payload).await.unwrap();
        let (tx, mut rx) = mpsc::channel::<u64>(8);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        session.upload_file(&tmp, &path, tx, Arc::new(std::sync::atomic::AtomicU8::new(0))).await.expect("upload");
        let _ = tokio::fs::remove_file(&tmp).await;
    }

    // The whole tree must delete in one call.
    session
        .remove_path(&base, RemoteFileKind::Directory)
        .await
        .expect("recursive remove should succeed on a non-empty dir");

    // And it must actually be gone — listing the parent should not contain it.
    let parent = session.list_dir("/tmp").await.expect("list /tmp");
    let still_there = parent.iter().any(|e| base.ends_with(&e.name));
    assert!(
        !still_there,
        "directory still present after recursive delete"
    );

    session.disconnect().await.expect("disconnect");
    eprintln!("OK: recursively deleted non-empty {base}");
}

/// Collect (first, last, monotonic) from a progress channel.
async fn drain_progress(mut rx: mpsc::Receiver<u64>) -> (u64, u64, bool) {
    let (mut first, mut last, mut monotonic) = (None, 0u64, true);
    while let Some(n) = rx.recv().await {
        if first.is_none() {
            first = Some(n);
        }
        if n < last {
            monotonic = false;
        }
        last = n;
    }
    (first.unwrap_or(0), last, monotonic)
}

/// A download whose local `.part` already holds the first half must RESUME from
/// that offset (not restart), append the remainder, and rename to the final.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumable_download_resumes_from_partial() {
    let Ok(password) = std::env::var("OPENTERM_TEST_PASSWORD") else {
        eprintln!("skipping: OPENTERM_TEST_PASSWORD not set");
        return;
    };
    let session = RusshBackend
        .connect_with_route(route(password))
        .await
        .expect("connect");

    let pid = std::process::id();
    let payload: Vec<u8> = (0..600 * 1024).map(|i| (i % 251) as u8).collect();
    let remote = format!("/tmp/openterm_resumedl_{pid}.bin");
    session
        .write_file(&remote, payload.clone())
        .await
        .expect("seed remote");

    // Simulate an interrupted download: a local `.part` holding the first half.
    let local = std::env::temp_dir().join(format!("openterm_resumedl_{pid}.bin"));
    let part = std::env::temp_dir().join(format!("openterm_resumedl_{pid}.bin.part"));
    let half = payload.len() / 2;
    tokio::fs::write(&part, &payload[..half]).await.unwrap();

    let (tx, rx) = mpsc::channel::<u64>(256);
    let collect = tokio::spawn(drain_progress(rx));
    let got = session
        .download_file(&remote, &local, tx, Arc::new(std::sync::atomic::AtomicU8::new(0)))
        .await
        .expect("download");
    let (first, last, monotonic) = collect.await.unwrap();

    assert_eq!(got, payload.len() as u64, "reported size");
    assert!(
        first as usize >= half,
        "expected resume from >= {half}, but progress started at {first}"
    );
    assert_eq!(last, payload.len() as u64, "final progress == total");
    assert!(monotonic, "progress regressed");
    let final_bytes = tokio::fs::read(&local).await.expect("read final");
    assert_eq!(final_bytes, payload, "resumed download bytes differ");
    assert!(!part.exists(), ".part should have been renamed away");

    session
        .remove_path(&remote, RemoteFileKind::File)
        .await
        .ok();
    let _ = tokio::fs::remove_file(&local).await;
    session.disconnect().await.expect("disconnect");
    eprintln!("OK: download resumed from {half} → {} bytes", payload.len());
}

/// An upload whose remote `.part` already holds the first half must RESUME from
/// that offset and produce a correct final file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumable_upload_resumes_from_partial() {
    let Ok(password) = std::env::var("OPENTERM_TEST_PASSWORD") else {
        eprintln!("skipping: OPENTERM_TEST_PASSWORD not set");
        return;
    };
    let session = RusshBackend
        .connect_with_route(route(password))
        .await
        .expect("connect");

    let pid = std::process::id();
    let payload: Vec<u8> = (0..600 * 1024).map(|i| ((i * 7) % 251) as u8).collect();
    let local = std::env::temp_dir().join(format!("openterm_resumeup_{pid}.bin"));
    tokio::fs::write(&local, &payload).await.unwrap();

    let remote = format!("/tmp/openterm_resumeup_{pid}.bin");
    let part = format!("{remote}.part");
    let half = payload.len() / 2;
    // Seed the remote `.part` with the first half.
    session
        .write_file(&part, payload[..half].to_vec())
        .await
        .expect("seed remote part");

    let (tx, rx) = mpsc::channel::<u64>(256);
    let collect = tokio::spawn(drain_progress(rx));
    let sent = session
        .upload_file(&local, &remote, tx, Arc::new(std::sync::atomic::AtomicU8::new(0)))
        .await
        .expect("upload");
    let (first, last, monotonic) = collect.await.unwrap();

    assert_eq!(sent, payload.len() as u64, "reported size");
    assert!(
        first as usize >= half,
        "expected resume from >= {half}, but progress started at {first}"
    );
    assert_eq!(last, payload.len() as u64, "final progress == total");
    assert!(monotonic, "progress regressed");
    let remote_bytes = session.read_file(&remote).await.expect("read remote final");
    assert_eq!(remote_bytes, payload, "resumed upload bytes differ");
    assert!(
        session.remote_file_size(&part).await.is_err(),
        "remote .part should have been renamed away"
    );

    session
        .remove_path(&remote, RemoteFileKind::File)
        .await
        .ok();
    let _ = tokio::fs::remove_file(&local).await;
    session.disconnect().await.expect("disconnect");
    eprintln!("OK: upload resumed from {half} → {} bytes", payload.len());
}

/// A large download must reassemble correctly even though chunks complete out
/// of order across the pipelined read window, with monotonic progress.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pipelined_download_large_file() {
    let Ok(password) = std::env::var("OPENTERM_TEST_PASSWORD") else {
        eprintln!("skipping: OPENTERM_TEST_PASSWORD not set");
        return;
    };
    let session = RusshBackend
        .connect_with_route(route(password))
        .await
        .expect("connect");

    let pid = std::process::id();
    // 4 MiB spans 16 × 256 KiB chunks → fills the whole read window.
    let payload: Vec<u8> = (0..4 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let remote = format!("/tmp/openterm_bigdl_{pid}.bin");
    session
        .write_file(&remote, payload.clone())
        .await
        .expect("seed remote");

    let local = std::env::temp_dir().join(format!("openterm_bigdl_{pid}.bin"));
    let _ = tokio::fs::remove_file(&local).await;
    let _ = tokio::fs::remove_file(std::env::temp_dir().join(format!("openterm_bigdl_{pid}.bin.part"))).await;

    let (tx, rx) = mpsc::channel::<u64>(256);
    let collect = tokio::spawn(drain_progress(rx));
    let got = session
        .download_file(&remote, &local, tx, Arc::new(std::sync::atomic::AtomicU8::new(0)))
        .await
        .expect("download");
    let (_first, last, monotonic) = collect.await.unwrap();

    assert_eq!(got, payload.len() as u64, "reported size");
    assert_eq!(last, payload.len() as u64, "final progress == total");
    assert!(monotonic, "pipelined progress regressed");
    let bytes = tokio::fs::read(&local).await.expect("read final");
    assert_eq!(bytes.len(), payload.len(), "size differs");
    assert_eq!(bytes, payload, "pipelined download corrupted bytes");

    session
        .remove_path(&remote, RemoteFileKind::File)
        .await
        .ok();
    let _ = tokio::fs::remove_file(&local).await;
    session.disconnect().await.expect("disconnect");
    eprintln!(
        "OK: pipelined download of {} MiB reassembled correctly",
        payload.len() / 1024 / 1024
    );
}
