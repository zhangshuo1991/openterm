//! Empirical stability probes for the two field reports:
//!
//! 1. **Large SFTP transfers drop after a while.** Reproduced by
//!    `download_and_upload_across_a_slow_link`: a rate-limited TCP proxy stands
//!    in for a modest cloud VM uplink, and the transfer must still complete.
//! 2. **An idle terminal session drops with no activity.** Reproduced by
//!    `idle_shell_survives_without_activity`: hold a PTY shell open and send
//!    nothing for five minutes.
//!
//! Both run against the loopback sshd described by
//! `~/.openterm-sshd-test/sshd_config` (127.0.0.1:2222, key auth) and use the
//! exact client configuration the app builds, so a pass here is a real
//! statement about the product, not about a toy harness.
//!
//! ```sh
//! # start the fixture sshd once
//! mkdir -p /tmp/openterm-sshd-test
//! /usr/sbin/sshd -f ~/.openterm-sshd-test/sshd_config -E /tmp/openterm-sshd-test/sshd.log
//!
//! # run the probes (they are long, so they are #[ignore]d by default)
//! cargo test -p openterm-ssh --test stability_probe -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Run with `RUST_LOG`-style verbosity via `OPENTERM_PROBE_LOG=1` to see russh's
//! own keepalive/disconnect diagnostics on stderr.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU8;
use std::sync::Arc;
use std::time::{Duration, Instant};

use openterm_core::HostProfile;
use openterm_ssh::{
    AuthMethod, ConnectOptions, ConnectRoute, HostKeyPolicy, PoolConfig, PtyEvent, PtyInput,
    PtySize, RusshBackend, ShellOptions, SshConnectionPool,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

const SSHD_PORT: u16 = 2222;

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

/// Minimal `log` sink so russh's internal `debug!`/`warn!` lines (keepalive
/// timeouts, disconnects, SFTP "unknown recipient") land on stderr with a
/// timestamp — the only way to tell *why* a probe failed.
struct StderrLogger {
    start: Instant,
    enabled: bool,
}

impl log::Log for StderrLogger {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        self.enabled
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled {
            eprintln!(
                "[{:>7.3}s {:>5}] {}",
                self.start.elapsed().as_secs_f64(),
                record.level(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

fn install_logger() {
    let start = Instant::now();
    let enabled = std::env::var("OPENTERM_PROBE_LOG").is_ok();
    let logger = Box::leak(Box::new(StderrLogger { start, enabled }));
    let _ = log::set_logger(logger);
    log::set_max_level(log::LevelFilter::Debug);
}

fn route_to(port: u16) -> ConnectRoute {
    route_with_keepalive(port, Some(ConnectOptions::DEFAULT_KEEPALIVE_INTERVAL))
}

fn route_with_keepalive(port: u16, keepalive_interval: Option<Duration>) -> ConnectRoute {
    let home = std::env::var("HOME").expect("HOME");
    let user = std::env::var("USER").unwrap_or_else(|_| "testuser".to_string());
    let mut profile = HostProfile::new("probe", "127.0.0.1");
    profile.port = port;
    profile.username = Some(user.clone());
    ConnectRoute {
        target: profile,
        target_options: ConnectOptions {
            username: user,
            auth: AuthMethod::PrivateKey {
                path: PathBuf::from(format!("{home}/.openterm-sshd-test/user_key")),
                passphrase: None,
            },
            trust_unknown_host_keys: true,
            host_key_policy: HostKeyPolicy::TrustAll,
            timeout: Duration::from_secs(15),
            keepalive_interval,
            keepalive_max: ConnectOptions::DEFAULT_KEEPALIVE_MAX,
        },
        jump: None,
    }
}

fn probe_dir() -> PathBuf {
    std::env::temp_dir().join(format!("openterm-stability-{}", std::process::id()))
}

/// Write `len` bytes of deterministic, position-dependent content. A repeating
/// pattern would hide offset bugs, so every 64 KiB block mixes in its index.
fn write_fixture(path: &Path, len: u64) {
    use std::io::Write;
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() == len {
            return;
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture dir");
    }
    let file = std::fs::File::create(path).expect("create fixture");
    let mut writer = std::io::BufWriter::with_capacity(1 << 20, file);
    let mut block = vec![0u8; 64 * 1024];
    let mut written = 0_u64;
    let mut index = 0_u64;
    while written < len {
        for (i, byte) in block.iter_mut().enumerate() {
            *byte = ((index as usize + i) % 251) as u8;
        }
        let take = ((len - written) as usize).min(block.len());
        writer.write_all(&block[..take]).expect("write fixture");
        written += take as u64;
        index += 1;
    }
    writer.flush().expect("flush fixture");
}

/// (length, FNV-1a hash) — cheap integrity check for multi-GB files.
fn fingerprint(path: &Path) -> (u64, u64) {
    use std::io::Read;
    let mut file = std::fs::File::open(path).expect("open for fingerprint");
    let mut buf = vec![0u8; 1 << 20];
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut len = 0_u64;
    loop {
        let read = file.read(&mut buf).expect("read for fingerprint");
        if read == 0 {
            break;
        }
        len += read as u64;
        for byte in &buf[..read] {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    (len, hash)
}

/// Token-bucket byte pump: average `bytes_per_sec`, with a bounded burst
/// allowance so handshake-sized control traffic is not delayed.
///
/// The bucket capacity matters: credit must NOT accumulate while a direction is
/// idle. An earlier version computed the allowance from "time since the pump
/// started", so after a 40s download the upload direction had banked 40s of
/// tokens and pushed 4 MiB instantly — the harness silently stopped modelling a
/// slow link, and a transfer that never streamed looked like a success.
async fn pump_limited<R, W>(
    label: &'static str,
    mut reader: R,
    mut writer: W,
    bytes_per_sec: u64,
) -> u64
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if bytes_per_sec == 0 {
        let _ = tokio::io::copy(&mut reader, &mut writer).await;
        return 0;
    }
    let rate = bytes_per_sec as f64;
    let capacity = (bytes_per_sec / 4).max(64 * 1024) as f64;
    let mut tokens = capacity;
    let mut last = Instant::now();
    let started = Instant::now();
    let mut total = 0_u64;
    let mut next_report = 1024 * 1024;
    let mut buf = vec![0u8; 32 * 1024];
    loop {
        let read = match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        // Refill from elapsed time, but never past the bucket capacity.
        let now = Instant::now();
        tokens = (tokens + now.duration_since(last).as_secs_f64() * rate).min(capacity);
        last = now;
        tokens -= read as f64;
        if tokens < 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(-tokens / rate)).await;
            tokens = 0.0;
            last = Instant::now();
        }
        total += read as u64;
        if total >= next_report {
            eprintln!(
                "    [{label}] {:.1} MiB at {:.2}s ({:.0} KiB/s)",
                total as f64 / (1024.0 * 1024.0),
                started.elapsed().as_secs_f64(),
                total as f64 / 1024.0 / started.elapsed().as_secs_f64().max(1e-9)
            );
            next_report += 1024 * 1024;
        }
        if writer.write_all(&buf[..read]).await.is_err() {
            break;
        }
    }
    let _ = writer.shutdown().await;
    total
}

/// A TCP proxy in front of the fixture sshd that throttles each direction, so a
/// loopback transfer can be made to look like a cloud VM on a small uplink.
async fn spawn_throttled_proxy(
    listen_port: u16,
    downstream_bps: u64,
    upstream_bps: u64,
) -> tokio::task::JoinHandle<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", listen_port))
        .await
        .expect("bind proxy");
    tokio::spawn(async move {
        loop {
            let Ok((mut client, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let Ok(mut server) = tokio::net::TcpStream::connect(("127.0.0.1", SSHD_PORT)).await
                else {
                    return;
                };
                let (client_read, client_write) = client.split();
                let (server_read, server_write) = server.split();
                let downlink =
                    pump_limited("server->client", server_read, client_write, downstream_bps);
                let uplink =
                    pump_limited("client->server", client_read, server_write, upstream_bps);
                tokio::join!(downlink, uplink);
            });
        }
    })
}

async fn connect(port: u16) -> Arc<openterm_ssh::RusshSession> {
    connect_with_keepalive(port, Some(ConnectOptions::DEFAULT_KEEPALIVE_INTERVAL)).await
}

async fn connect_with_keepalive(
    port: u16,
    keepalive_interval: Option<Duration>,
) -> Arc<openterm_ssh::RusshSession> {
    Arc::new(
        RusshBackend
            .connect_with_route(route_with_keepalive(port, keepalive_interval))
            .await
            .expect("connect to fixture sshd"),
    )
}

/// Drain progress so the reporter never blocks on a full channel, and return
/// the last cumulative byte count.
fn drain_progress() -> (mpsc::Sender<u64>, tokio::task::JoinHandle<u64>) {
    let (tx, mut rx) = mpsc::channel::<u64>(256);
    let task = tokio::spawn(async move {
        let mut last = 0_u64;
        while let Some(value) = rx.recv().await {
            last = value;
        }
        last
    });
    (tx, task)
}

fn stop_flag() -> Arc<AtomicU8> {
    Arc::new(AtomicU8::new(0))
}

/// Send `echo <marker>` to the shell and wait for the marker to come back.
async fn shell_echoes(
    input: &mpsc::Sender<PtyInput>,
    events: &mut mpsc::Receiver<PtyEvent>,
    marker: &str,
    within: Duration,
) -> bool {
    if input
        .send(PtyInput::Write(format!("echo {marker}\n").into_bytes()))
        .await
        .is_err()
    {
        return false;
    }
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), events.recv()).await {
            Ok(Some(PtyEvent::Output(bytes))) => {
                if String::from_utf8_lossy(&bytes).contains(marker) {
                    return true;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => return false,
            Err(_) => {}
        }
    }
    false
}

// ---------------------------------------------------------------------------
// probe 1 — the idle terminal must survive
// ---------------------------------------------------------------------------

/// Field report 2: "a while with no activity in the terminal window and SSH
/// drops on its own".
///
/// Holds a PTY shell open for [`IDLE_WINDOW`] without sending a single byte and
/// requires the channel to still be usable afterwards. If the session dies, the
/// shell task finishes early (russh closes the channel), so the timeout is the
/// assertion.
const IDLE_WINDOW: Duration = Duration::from_secs(300);

async fn assert_idle_survival(keepalive_interval: Option<Duration>, label: &str) {
    install_logger();
    let session = connect_with_keepalive(SSHD_PORT, keepalive_interval).await;
    let (in_tx, mut in_rx) = mpsc::channel::<PtyInput>(64);
    let (ev_tx, mut ev_rx) = mpsc::channel::<PtyEvent>(256);
    let shell_session = session.clone();
    let mut shell = tokio::spawn(async move {
        shell_session
            .event_shell(
                ShellOptions {
                    term: "xterm-256color".to_string(),
                    size: PtySize {
                        cols: 100,
                        rows: 30,
                    },
                    command: None,
                },
                &mut in_rx,
                ev_tx,
            )
            .await
    });

    // Confirm the shell is actually up before going quiet.
    in_tx
        .send(PtyInput::Write(b"echo READY\n".to_vec()))
        .await
        .expect("send READY");
    let mut ready = false;
    let ready_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !ready && tokio::time::Instant::now() < ready_deadline {
        if let Ok(Some(PtyEvent::Output(bytes))) =
            tokio::time::timeout(Duration::from_secs(2), ev_rx.recv()).await
        {
            ready = String::from_utf8_lossy(&bytes).contains("READY");
        }
    }
    assert!(ready, "shell never echoed READY; fixture sshd not usable");

    // The idle window: absolutely no traffic from the client.
    eprintln!(
        "{label}: idle for {}s with no traffic ...",
        IDLE_WINDOW.as_secs()
    );
    let idle_start = Instant::now();
    match tokio::time::timeout(IDLE_WINDOW, &mut shell).await {
        Ok(joined) => {
            let elapsed = idle_start.elapsed().as_secs_f64();
            panic!(
                "{label}: session died after {elapsed:.1}s of inactivity (shell task returned \
                 {joined:?}); an idle terminal must stay connected"
            );
        }
        Err(_) => eprintln!(
            "{label}: still connected after {:.1}s idle",
            idle_start.elapsed().as_secs_f64()
        ),
    }

    // And it must still work, not merely look open.
    in_tx
        .send(PtyInput::Write(b"echo ALIVE_AFTER_IDLE\n".to_vec()))
        .await
        .expect("send ALIVE_AFTER_IDLE");
    let mut alive = false;
    let alive_deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !alive && tokio::time::Instant::now() < alive_deadline {
        if let Ok(Some(PtyEvent::Output(bytes))) =
            tokio::time::timeout(Duration::from_secs(5), ev_rx.recv()).await
        {
            alive = String::from_utf8_lossy(&bytes).contains("ALIVE_AFTER_IDLE");
        }
    }
    assert!(
        alive,
        "{label}: session survived the idle window but the shell is no longer responsive"
    );

    let _ = session.disconnect().await;
    shell.abort();
}

/// Baseline: SSH-level keepalives on (the app's default), which this probe
/// confirmed are sent every 30s and answered by the server.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long-running: holds a session idle for 5 minutes"]
async fn idle_shell_survives_without_activity() {
    assert_idle_survival(
        Some(ConnectOptions::DEFAULT_KEEPALIVE_INTERVAL),
        "ssh-keepalive",
    )
    .await;
}

/// The hard case, and the one that matches the field report: the SSH keepalive is
/// a *user-space* timer, so it stops firing when the app is throttled — macOS App
/// Nap does exactly that once the window has been left alone for a while. With
/// SSH keepalives switched off, nothing but the kernel's TCP keepalives keeps the
/// flow alive, so this passing is what makes "walked away, came back, session
/// still there" true even when the app itself was not scheduled.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long-running: holds a session idle for 5 minutes with no user-space keepalives"]
async fn idle_shell_survives_on_tcp_keepalives_alone() {
    assert_idle_survival(None, "tcp-keepalive-only").await;
}

// ---------------------------------------------------------------------------
// probe 2 — transfers must survive a modest uplink
// ---------------------------------------------------------------------------

/// Field report 1: "large SFTP transfers drop the connection after a while".
///
/// This is the reproduction. A proxy in front of the sshd caps the link at
/// 800 kbit/s down / 800 kbit/s up (a small cloud VM's realistic sustained
/// throughput, and far better than a congested cross-border link), then a 4 MiB
/// file is downloaded and uploaded through it.
///
/// The transfer must complete: a slow link is not an error condition. The probe
/// fails loudly, with the underlying `SshError`, when it does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long-running: 4 MiB each way over a throttled link"]
async fn transfer_completes_across_a_slow_link() {
    const LINK_BPS: u64 = 100 * 1024; // ~800 kbit/s each way
    const SIZE: u64 = 4 * 1024 * 1024;
    install_logger();

    // Start from a clean directory. A leftover `.part` of the full size makes a
    // "resumed" transfer look like an instant success, which would hide exactly
    // the failure this probe exists to catch.
    let dir = probe_dir();
    let _ = std::fs::remove_dir_all(&dir);
    let source = dir.join("slow-source.bin");
    let downloaded = dir.join("slow-downloaded.bin");
    let upload_source = dir.join("slow-upload-source.bin");
    let uploaded = dir.join("slow-uploaded.bin");
    write_fixture(&source, SIZE);
    write_fixture(&upload_source, SIZE);
    let expected = fingerprint(&source);
    // Half the theoretical link time: a real 4 MiB transfer at 800 kbit/s cannot
    // finish faster, so this catches a transfer that only renamed a scratch file.
    let link_floor = Duration::from_secs_f64(SIZE as f64 / (2.0 * LINK_BPS as f64));

    let proxy = spawn_throttled_proxy(2301, LINK_BPS, LINK_BPS).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let session = connect(2301).await;

    let (dtx, dprogress) = drain_progress();
    let started = Instant::now();
    let result = session
        .download_file(&source.to_string_lossy(), &downloaded, dtx, stop_flag())
        .await;
    let elapsed = started.elapsed();
    let final_bytes = dprogress.await.unwrap_or(0);
    match result {
        Ok(bytes) => {
            assert_eq!(bytes, SIZE, "download reported the wrong byte count");
            assert_eq!(
                final_bytes, SIZE,
                "download progress never reached the file size"
            );
            assert!(
                elapsed >= link_floor,
                "download finished in {:.1}s, faster than {SIZE} bytes can cross a {LINK_BPS} B/s link; \
                 it did not actually stream",
                elapsed.as_secs_f64()
            );
            assert_eq!(
                fingerprint(&downloaded),
                expected,
                "downloaded bytes differ from the source"
            );
            eprintln!(
                "download OK: {SIZE} bytes in {:.1}s ({:.0} KiB/s)",
                elapsed.as_secs_f64(),
                SIZE as f64 / 1024.0 / elapsed.as_secs_f64()
            );
        }
        Err(error) => panic!(
            "download of {SIZE} bytes over a {LINK_BPS} B/s link failed after {:.1}s: {error}",
            elapsed.as_secs_f64()
        ),
    }

    // Timestamped progress: if every update lands in the same instant, the
    // "upload" never crossed the link and the elapsed time below is meaningless.
    let (utx, mut urx) = mpsc::channel::<u64>(256);
    let started = Instant::now();
    let trace = tokio::spawn(async move {
        let mut last = 0_u64;
        let mut events = 0_usize;
        while let Some(value) = urx.recv().await {
            events += 1;
            eprintln!(
                "    upload progress #{events} at {:.2}s: {value} bytes",
                started.elapsed().as_secs_f64()
            );
            last = value;
        }
        (last, events)
    });
    let result = session
        .upload_file(
            &upload_source,
            &uploaded.to_string_lossy(),
            utx,
            stop_flag(),
        )
        .await;
    let elapsed = started.elapsed();
    let (final_bytes, progress_events) = trace.await.unwrap_or((0, 0));
    eprintln!(
        "    upload_file returned after {:.2}s with {progress_events} progress events",
        elapsed.as_secs_f64()
    );
    match result {
        Ok(bytes) => {
            assert_eq!(bytes, SIZE, "upload reported the wrong byte count");
            assert_eq!(
                final_bytes, SIZE,
                "upload progress never reached the file size"
            );
            assert!(
                elapsed >= link_floor,
                "upload finished in {:.1}s, faster than {SIZE} bytes can cross a {LINK_BPS} B/s link; \
                 it did not actually stream",
                elapsed.as_secs_f64()
            );
            assert_eq!(
                fingerprint(&uploaded),
                expected,
                "uploaded bytes differ from the source"
            );
            eprintln!(
                "upload OK: {SIZE} bytes in {:.1}s ({:.0} KiB/s)",
                elapsed.as_secs_f64(),
                SIZE as f64 / 1024.0 / elapsed.as_secs_f64()
            );
        }
        Err(error) => panic!(
            "upload of {SIZE} bytes over a {LINK_BPS} B/s link failed after {:.1}s: {error}",
            elapsed.as_secs_f64()
        ),
    }

    // The terminal must have survived the transfer: that is the "connection
    // dropped" half of the report.
    assert!(
        session.is_alive().await,
        "session was unusable after the transfers"
    );

    let _ = session.disconnect().await;
    proxy.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The same transfers over loopback, where the link is never the limit. A
/// multi-GB payload crosses OpenSSH's rekey boundary, which is the other place
/// long transfers die.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long-running: writes ~2.5 GiB of scratch files"]
async fn transfer_completes_over_loopback_across_a_rekey() {
    const SIZE: u64 = 1536 * 1024 * 1024;
    install_logger();

    let dir = probe_dir();
    let source = dir.join("loop-source.bin");
    let downloaded = dir.join("loop-downloaded.bin");
    write_fixture(&source, SIZE);
    let _ = std::fs::remove_file(&downloaded);
    let _ = std::fs::remove_file(&format!("{}.part", downloaded.display()));
    let expected = fingerprint(&source);

    let session = connect(SSHD_PORT).await;
    let (dtx, dprogress) = drain_progress();
    let started = Instant::now();
    let result = session
        .download_file(&source.to_string_lossy(), &downloaded, dtx, stop_flag())
        .await;
    let elapsed = started.elapsed();
    let _ = dprogress.await;
    let bytes = result.unwrap_or_else(|error| {
        panic!(
            "loopback download of {SIZE} bytes failed after {:.1}s: {error}",
            elapsed.as_secs_f64()
        )
    });
    assert_eq!(bytes, SIZE);
    assert_eq!(
        fingerprint(&downloaded),
        expected,
        "downloaded bytes differ"
    );
    eprintln!(
        "loopback download OK: {SIZE} bytes in {:.1}s ({:.0} MiB/s)",
        elapsed.as_secs_f64(),
        SIZE as f64 / 1024.0 / 1024.0 / elapsed.as_secs_f64()
    );

    let _ = session.disconnect().await;
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// probe 4 — bulk transfers must not share the terminal's connection
// ---------------------------------------------------------------------------

/// The structural claim behind "an SFTP transfer dropped my connection": bulk
/// transfers run on their own pooled connection, so a transfer that dies cannot
/// take the interactive session with it. Requires the fixture sshd.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the loopback sshd fixture"]
async fn transfer_connection_is_separate_and_its_death_spares_the_terminal() {
    install_logger();
    let pool = Arc::new(
        SshConnectionPool::new(route_to(SSHD_PORT), PoolConfig::default())
            .await
            .expect("create pool"),
    );
    let primary = pool.primary();

    // A shell on the primary connection, exactly like the app's terminal.
    let (in_tx, mut in_rx) = mpsc::channel::<PtyInput>(64);
    let (ev_tx, mut ev_rx) = mpsc::channel::<PtyEvent>(256);
    let shell_session = primary.clone();
    let shell = tokio::spawn(async move {
        shell_session
            .event_shell(
                ShellOptions {
                    term: "xterm-256color".to_string(),
                    size: PtySize {
                        cols: 100,
                        rows: 30,
                    },
                    command: None,
                },
                &mut in_rx,
                ev_tx,
            )
            .await
    });
    assert!(
        shell_echoes(&in_tx, &mut ev_rx, "READY", Duration::from_secs(10)).await,
        "shell never came up"
    );

    // A transfer borrows a different connection.
    let data = pool
        .acquire_data_connection()
        .await
        .expect("acquire data connection");
    assert!(
        !Arc::ptr_eq(&primary, data.session()),
        "a bulk transfer must not run on the terminal's connection"
    );
    assert!(
        data.session().is_alive().await,
        "data connection is not usable"
    );

    // Kill it the way a dropped link would, then hand it back to the pool.
    let doomed = data.session().clone();
    let _ = doomed.disconnect().await;
    drop(data);
    tokio::time::sleep(Duration::from_millis(300)).await;

    // The terminal is untouched...
    assert!(
        primary.is_alive().await,
        "the terminal's connection must survive a transfer connection dying"
    );
    assert!(
        shell_echoes(
            &in_tx,
            &mut ev_rx,
            "TERMINAL_ALIVE",
            Duration::from_secs(15)
        )
        .await,
        "the terminal stopped working after a transfer connection died"
    );

    // ...and the pool must not hand the dead connection to the next transfer.
    let replacement = pool
        .acquire_data_connection()
        .await
        .expect("re-acquire data connection");
    assert!(
        replacement.session().is_alive().await,
        "the pool handed out a dead connection"
    );
    assert!(
        !Arc::ptr_eq(replacement.session(), &doomed),
        "the pool reused the connection that was just killed"
    );

    let _ = shell.abort();
    let _ = pool.shutdown().await;
}

/// A returned connection is reused rather than redialled, and the pool's
/// capacity is a real cap on live connections.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the loopback sshd fixture"]
async fn pool_reuses_data_connections_and_enforces_its_cap() {
    install_logger();
    let config = PoolConfig {
        max_data_connections: 2,
        acquire_timeout: Duration::from_millis(700),
        ..Default::default()
    };
    let pool = Arc::new(
        SshConnectionPool::new(route_to(SSHD_PORT), config)
            .await
            .expect("create pool"),
    );

    let first = pool.acquire_data_connection().await.expect("first acquire");
    let first_session = first.session().clone();
    drop(first);
    // The return happens in a spawned task, so let it land.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let second = pool
        .acquire_data_connection()
        .await
        .expect("second acquire");
    assert!(
        Arc::ptr_eq(second.session(), &first_session),
        "a returned connection must be reused, not redialled"
    );

    let third = pool.acquire_data_connection().await.expect("third acquire");
    let fourth = pool.acquire_data_connection().await;
    match fourth {
        Ok(_) => panic!("a fourth connection exceeded the pool's capacity of 2"),
        Err(error) => {
            let text = error.to_string();
            assert!(
                text.contains("busy"),
                "the cap must be reported as a busy pool, got: {text}"
            );
            eprintln!("cap enforced with: {text}");
        }
    }

    drop(second);
    drop(third);
    let stats = pool.stats().await;
    assert_eq!(
        stats.total_created, 2,
        "exactly two connections were dialled"
    );
    assert!(
        stats.total_reused >= 1,
        "the returned connection was not reused"
    );
    let _ = pool.shutdown().await;
}
