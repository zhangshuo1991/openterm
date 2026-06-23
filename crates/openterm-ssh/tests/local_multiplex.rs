//! Local multiplex test against localhost:2222 with key auth.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use openterm_core::HostProfile;
use openterm_ssh::{
    AuthMethod, ConnectOptions, ConnectRoute, HostKeyPolicy, PtyEvent, PtyInput, PtySize,
    RusshBackend, ShellOptions,
};
use tokio::sync::mpsc;

fn route() -> ConnectRoute {
    let home = std::env::var("HOME").expect("HOME");
    let mut profile = HostProfile::new("local", "127.0.0.1");
    profile.port = 2222;
    profile.username = Some(std::env::var("USER").unwrap_or_else(|_| "testuser".to_string()));
    ConnectRoute {
        target: profile,
        target_options: ConnectOptions {
            username: std::env::var("USER").unwrap_or_else(|_| "testuser".to_string()),
            auth: AuthMethod::PrivateKey {
                path: PathBuf::from(format!("{home}/.openterm-sshd-test/user_key")),
                passphrase: None,
            },
            trust_unknown_host_keys: true,
            host_key_policy: HostKeyPolicy::TrustAll,
            timeout: Duration::from_secs(15),
        },
        jump: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_and_sftp_multiplex_on_local_connection() {
    let session = Arc::new(
        RusshBackend
            .connect_with_route(route())
            .await
            .expect("connect"),
    );

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

    // Wait a moment for shell to be ready, then list dir over SFTP on the same connection.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let sftp_session = session.clone();
    let listing = tokio::spawn(async move { sftp_session.list_dir(".").await });

    let marker = format!("OPENTERM_LOCAL_{}", std::process::id());
    in_tx
        .send(PtyInput::Write(format!("echo {marker}\n").into_bytes()))
        .await
        .unwrap();

    let mut shell_saw_marker = false;
    let mut shell_closed = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), ev_rx.recv()).await {
            Ok(Some(PtyEvent::Output(bytes))) => {
                let text = String::from_utf8_lossy(&bytes);
                eprintln!("shell output: {:?}", text);
                if text.contains(&marker) {
                    shell_saw_marker = true;
                    break;
                }
            }
            Ok(Some(PtyEvent::Closed)) | Ok(Some(PtyEvent::ExitStatus(_))) => {
                eprintln!("shell CLOSED while SFTP was open");
                shell_closed = true;
                break;
            }
            Ok(None) => {
                eprintln!("shell event channel closed");
                shell_closed = true;
                break;
            }
            Err(_) => {}
        }
    }

    let sftp_result = listing.await.expect("sftp task");
    let entries = sftp_result.expect("sftp list ok");

    session.disconnect().await.expect("disconnect");
    let _ = in_tx;
    let _ = shell.await;

    assert!(!shell_closed, "shell closed while SFTP list was in flight");
    assert!(shell_saw_marker, "shell did not echo marker over PTY");
    assert!(!entries.is_empty(), "sftp listing was empty");
    eprintln!(
        "OK: shell PTY echoed marker AND sftp listed {} entries on one connection",
        entries.len()
    );
}
