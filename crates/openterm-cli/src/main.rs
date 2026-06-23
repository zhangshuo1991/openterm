use anyhow::Context;
use clap::{Parser, Subcommand};
use directories::ProjectDirs;
use openterm_config::{export_hosts_toml, import_openssh_config};
use openterm_core::HostProfile;
use openterm_ssh::{
    AuthMethod, ConnectOptions, ForwardEvent, HostKeyPolicy, LocalForwardOptions, PtySize,
    RemoteFileEntry, RemoteFileKind, RusshBackend, ShellOptions,
};
use openterm_storage::WorkspaceStore;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Parser)]
#[command(name = "openterm")]
#[command(about = "Local-first OpenTerm workspace CLI")]
struct Cli {
    #[arg(long)]
    db: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    ListHosts,
    AddHost {
        name: String,
        host: String,
        #[arg(long)]
        user: Option<String>,
        #[arg(long, default_value_t = 22)]
        port: u16,
    },
    ImportSshConfig {
        path: PathBuf,
    },
    ExportToml,
    Exec {
        target: String,
        command: String,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        password_env: Option<String>,
        #[arg(long)]
        password_stdin: bool,
        #[arg(long)]
        key: Option<PathBuf>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(long, default_value_t = true)]
        trust_unknown_host_keys: bool,
    },
    Shell {
        target: String,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        password_env: Option<String>,
        #[arg(long)]
        password_stdin: bool,
        #[arg(long)]
        key: Option<PathBuf>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(long, default_value_t = true)]
        trust_unknown_host_keys: bool,
        #[arg(long)]
        term: Option<String>,
        #[arg(long, default_value_t = 80)]
        cols: u16,
        #[arg(long, default_value_t = 24)]
        rows: u16,
    },
    SftpList {
        target: String,
        path: String,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        password_env: Option<String>,
        #[arg(long)]
        password_stdin: bool,
        #[arg(long)]
        key: Option<PathBuf>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(long, default_value_t = true)]
        trust_unknown_host_keys: bool,
    },
    SftpDownload {
        target: String,
        remote_path: String,
        local_path: PathBuf,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        password_env: Option<String>,
        #[arg(long)]
        password_stdin: bool,
        #[arg(long)]
        key: Option<PathBuf>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(long, default_value_t = true)]
        trust_unknown_host_keys: bool,
    },
    SftpUpload {
        target: String,
        local_path: PathBuf,
        remote_path: String,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        password_env: Option<String>,
        #[arg(long)]
        password_stdin: bool,
        #[arg(long)]
        key: Option<PathBuf>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(long, default_value_t = true)]
        trust_unknown_host_keys: bool,
    },
    SftpMkdir {
        target: String,
        remote_path: String,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        password_env: Option<String>,
        #[arg(long)]
        password_stdin: bool,
        #[arg(long)]
        key: Option<PathBuf>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(long, default_value_t = true)]
        trust_unknown_host_keys: bool,
    },
    SftpRm {
        target: String,
        remote_path: String,
        #[arg(long)]
        dir: bool,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        password_env: Option<String>,
        #[arg(long)]
        password_stdin: bool,
        #[arg(long)]
        key: Option<PathBuf>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(long, default_value_t = true)]
        trust_unknown_host_keys: bool,
    },
    SftpRename {
        target: String,
        old_path: String,
        new_path: String,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        password_env: Option<String>,
        #[arg(long)]
        password_stdin: bool,
        #[arg(long)]
        key: Option<PathBuf>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(long, default_value_t = true)]
        trust_unknown_host_keys: bool,
    },
    ForwardLocal {
        target: String,
        #[arg(long, default_value = "127.0.0.1")]
        bind_host: String,
        #[arg(long)]
        bind_port: u16,
        #[arg(long)]
        remote_host: String,
        #[arg(long)]
        remote_port: u16,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        password_env: Option<String>,
        #[arg(long)]
        password_stdin: bool,
        #[arg(long)]
        key: Option<PathBuf>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(long, default_value_t = true)]
        trust_unknown_host_keys: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let store = WorkspaceStore::open(cli.db.unwrap_or_else(default_db_path))?;

    match cli.command {
        Command::ListHosts => {
            for host in store.list_hosts()? {
                println!("{}\t{}\t{}", host.id, host.name, host.display_target());
            }
        }
        Command::AddHost {
            name,
            host,
            user,
            port,
        } => {
            let mut profile = HostProfile::new(name, host);
            profile.username = user;
            profile.port = port;
            store.save_host(&profile)?;
            println!("saved {}", profile.display_target());
        }
        Command::ImportSshConfig { path } => {
            let input = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let hosts = import_openssh_config(&input);
            for host in &hosts {
                store.save_host(host)?;
            }
            println!("imported {} hosts", hosts.len());
        }
        Command::ExportToml => {
            let hosts = store.list_hosts()?;
            print!("{}", export_hosts_toml(&hosts)?);
        }
        Command::Exec {
            target,
            command,
            user,
            password,
            password_env,
            password_stdin,
            key,
            port,
            trust_unknown_host_keys,
        } => {
            let (profile, options) = ssh_options(
                &store,
                &target,
                user,
                password,
                password_env,
                password_stdin,
                key,
                port,
                trust_unknown_host_keys,
            )?;
            let output = RusshBackend
                .exec_with_options(profile, options, &command)
                .await?;
            print!("{}", String::from_utf8_lossy(&output.stdout));
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
            std::process::exit(output.exit_status as i32);
        }
        Command::Shell {
            target,
            user,
            password,
            password_env,
            password_stdin,
            key,
            port,
            trust_unknown_host_keys,
            term,
            cols,
            rows,
        } => {
            let (profile, options) = ssh_options(
                &store,
                &target,
                user,
                password,
                password_env,
                password_stdin,
                key,
                port,
                trust_unknown_host_keys,
            )?;
            let _raw_mode = RawMode::enable().context("failed to enable terminal raw mode")?;
            let exit_status = RusshBackend
                .shell_with_options(
                    profile,
                    options,
                    ShellOptions {
                        term: term
                            .or_else(|| std::env::var("TERM").ok())
                            .unwrap_or_else(|| "xterm-256color".to_string()),
                        size: PtySize { cols, rows },
                    },
                    tokio::io::stdin(),
                    tokio::io::stdout(),
                )
                .await?;
            std::process::exit(exit_status as i32);
        }
        Command::SftpList {
            target,
            path,
            user,
            password,
            password_env,
            password_stdin,
            key,
            port,
            trust_unknown_host_keys,
        } => {
            let (profile, options) = ssh_options(
                &store,
                &target,
                user,
                password,
                password_env,
                password_stdin,
                key,
                port,
                trust_unknown_host_keys,
            )?;
            let entries = RusshBackend
                .list_dir_with_options(profile, options, &path)
                .await?;
            print_remote_entries(&entries);
        }
        Command::SftpDownload {
            target,
            remote_path,
            local_path,
            user,
            password,
            password_env,
            password_stdin,
            key,
            port,
            trust_unknown_host_keys,
        } => {
            let (profile, options) = ssh_options(
                &store,
                &target,
                user,
                password,
                password_env,
                password_stdin,
                key,
                port,
                trust_unknown_host_keys,
            )?;
            let bytes = RusshBackend
                .read_file_with_options(profile, options, &remote_path)
                .await?;
            tokio::fs::write(&local_path, bytes)
                .await
                .with_context(|| format!("failed to write {}", local_path.display()))?;
            println!("downloaded {remote_path} to {}", local_path.display());
        }
        Command::SftpUpload {
            target,
            local_path,
            remote_path,
            user,
            password,
            password_env,
            password_stdin,
            key,
            port,
            trust_unknown_host_keys,
        } => {
            let bytes = tokio::fs::read(&local_path)
                .await
                .with_context(|| format!("failed to read {}", local_path.display()))?;
            let (profile, options) = ssh_options(
                &store,
                &target,
                user,
                password,
                password_env,
                password_stdin,
                key,
                port,
                trust_unknown_host_keys,
            )?;
            RusshBackend
                .write_file_with_options(profile, options, &remote_path, bytes)
                .await?;
            println!("uploaded {} to {remote_path}", local_path.display());
        }
        Command::SftpMkdir {
            target,
            remote_path,
            user,
            password,
            password_env,
            password_stdin,
            key,
            port,
            trust_unknown_host_keys,
        } => {
            let (profile, options) = ssh_options(
                &store,
                &target,
                user,
                password,
                password_env,
                password_stdin,
                key,
                port,
                trust_unknown_host_keys,
            )?;
            RusshBackend
                .create_dir_with_options(profile, options, &remote_path)
                .await?;
            println!("created {remote_path}");
        }
        Command::SftpRm {
            target,
            remote_path,
            dir,
            user,
            password,
            password_env,
            password_stdin,
            key,
            port,
            trust_unknown_host_keys,
        } => {
            let (profile, options) = ssh_options(
                &store,
                &target,
                user,
                password,
                password_env,
                password_stdin,
                key,
                port,
                trust_unknown_host_keys,
            )?;
            let kind = if dir {
                RemoteFileKind::Directory
            } else {
                RemoteFileKind::File
            };
            RusshBackend
                .remove_path_with_options(profile, options, &remote_path, kind)
                .await?;
            println!("removed {remote_path}");
        }
        Command::SftpRename {
            target,
            old_path,
            new_path,
            user,
            password,
            password_env,
            password_stdin,
            key,
            port,
            trust_unknown_host_keys,
        } => {
            let (profile, options) = ssh_options(
                &store,
                &target,
                user,
                password,
                password_env,
                password_stdin,
                key,
                port,
                trust_unknown_host_keys,
            )?;
            RusshBackend
                .rename_path_with_options(profile, options, &old_path, &new_path)
                .await?;
            println!("renamed {old_path} to {new_path}");
        }
        Command::ForwardLocal {
            target,
            bind_host,
            bind_port,
            remote_host,
            remote_port,
            user,
            password,
            password_env,
            password_stdin,
            key,
            port,
            trust_unknown_host_keys,
        } => {
            let (profile, options) = ssh_options(
                &store,
                &target,
                user,
                password,
                password_env,
                password_stdin,
                key,
                port,
                trust_unknown_host_keys,
            )?;
            let (event_sender, mut event_receiver) = tokio::sync::mpsc::channel(100);
            let (_stop_sender, stop_receiver) = tokio::sync::mpsc::channel(1);
            let forward = LocalForwardOptions {
                bind_host,
                bind_port,
                remote_host,
                remote_port,
            };
            let mut runner = Box::pin(tokio::spawn(async move {
                RusshBackend
                    .run_local_forward_with_options(
                        profile,
                        options,
                        forward,
                        event_sender,
                        stop_receiver,
                    )
                    .await
            }));

            loop {
                tokio::select! {
                    event = event_receiver.recv() => {
                        match event {
                            Some(event) => println!("{}", format_forward_event(&event)),
                            None => break,
                        }
                    }
                    result = &mut runner => {
                        result??;
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

fn ssh_options(
    store: &WorkspaceStore,
    target: &str,
    user: Option<String>,
    password: Option<String>,
    password_env: Option<String>,
    password_stdin: bool,
    key: Option<PathBuf>,
    port: u16,
    trust_unknown_host_keys: bool,
) -> anyhow::Result<(HostProfile, ConnectOptions)> {
    let mut profile = resolve_target(store, target, port)?;
    if let Some(user) = user {
        profile.username = Some(user);
    }
    let username = profile
        .username
        .clone()
        .or_else(|| std::env::var("USER").ok())
        .context("username is required; pass --user or save it on the host")?;
    let env_password = password_env
        .as_deref()
        .map(|name| {
            std::env::var(name).with_context(|| format!("password env var {name} is not set"))
        })
        .transpose()?;
    let stdin_password = if password_stdin {
        Some(read_password_stdin()?)
    } else {
        None
    };
    let provided_passwords = [
        password.as_ref().map(|_| "--password"),
        env_password.as_ref().map(|_| "--password-env"),
        stdin_password.as_ref().map(|_| "--password-stdin"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if provided_passwords.len() > 1 {
        anyhow::bail!(
            "use only one password source: {}",
            provided_passwords.join(", ")
        );
    }
    if key.is_some() && !provided_passwords.is_empty() {
        anyhow::bail!("use either password auth or --key, not both");
    }

    let auth = match (password, env_password, stdin_password, key) {
        (Some(password), None, None, None) => AuthMethod::Password(password),
        (None, Some(password), None, None) => AuthMethod::Password(password),
        (None, None, Some(password), None) => AuthMethod::Password(password),
        (None, None, None, Some(path)) => AuthMethod::PrivateKey {
            path,
            passphrase: None,
        },
        (None, None, None, None) => AuthMethod::AgentOrDefault,
        _ => unreachable!("password and key combinations are validated above"),
    };

    Ok((
        profile,
        ConnectOptions {
            username,
            auth,
            trust_unknown_host_keys,
            host_key_policy: cli_host_key_policy(trust_unknown_host_keys),
            timeout: Duration::from_secs(10),
        },
    ))
}

fn read_password_stdin() -> anyhow::Result<String> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("failed to read password from stdin")?;
    Ok(input.trim_end_matches(['\r', '\n']).to_string())
}

struct RawMode {
    original: libc::termios,
}

impl RawMode {
    fn enable() -> anyhow::Result<Self> {
        let fd = libc::STDIN_FILENO;
        let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(fd, original.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error()).context("tcgetattr failed");
        }
        let original = unsafe { original.assume_init() };
        let mut raw = original;
        raw.c_iflag &= !(libc::BRKINT | libc::ICRNL | libc::INPCK | libc::ISTRIP | libc::IXON);
        raw.c_oflag &= !libc::OPOST;
        raw.c_cflag |= libc::CS8;
        raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::IEXTEN | libc::ISIG);
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw) } != 0 {
            return Err(std::io::Error::last_os_error()).context("tcsetattr failed");
        }
        Ok(Self { original })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &self.original) };
    }
}

fn resolve_target(
    store: &WorkspaceStore,
    target: &str,
    default_port: u16,
) -> anyhow::Result<HostProfile> {
    let hosts = store.list_hosts()?;
    if let Some(host) = hosts
        .iter()
        .find(|host| host.name == target || host.host == target || host.display_target() == target)
    {
        return Ok(host.clone());
    }

    let mut profile = HostProfile::new(target, target);
    profile.port = default_port;
    Ok(profile)
}

fn default_db_path() -> PathBuf {
    let dirs = ProjectDirs::from("dev", "OpenTerm", "OpenTerm")
        .expect("project directories should be available on desktop platforms");
    let data_dir = dirs.data_local_dir();
    let _ = std::fs::create_dir_all(data_dir);
    data_dir.join("openterm.redb")
}

fn print_remote_entries(entries: &[RemoteFileEntry]) {
    for entry in entries {
        println!(
            "{}\t{}\t{}\t{}",
            remote_file_kind_label(&entry.kind),
            entry
                .size
                .map(|size| size.to_string())
                .unwrap_or_else(|| "-".to_string()),
            entry
                .permissions
                .map(|mode| format!("{mode:o}"))
                .unwrap_or_else(|| "-".to_string()),
            entry.path
        );
    }
}

fn remote_file_kind_label(kind: &RemoteFileKind) -> &'static str {
    match kind {
        RemoteFileKind::Directory => "dir",
        RemoteFileKind::File => "file",
        RemoteFileKind::Symlink => "link",
        RemoteFileKind::Other => "other",
    }
}

fn cli_host_key_policy(trust_unknown_host_keys: bool) -> HostKeyPolicy {
    if trust_unknown_host_keys {
        HostKeyPolicy::TrustAll
    } else {
        HostKeyPolicy::AcceptNew {
            known_hosts: default_known_hosts_path(),
        }
    }
}

fn default_known_hosts_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ssh")
        .join("known_hosts")
}

fn format_forward_event(event: &ForwardEvent) -> String {
    match event {
        ForwardEvent::Listening {
            bind_host,
            bind_port,
        } => {
            format!("listening {bind_host}:{bind_port}")
        }
        ForwardEvent::ConnectionAccepted { peer } => format!("accepted {peer}"),
        ForwardEvent::ConnectionClosed { peer } => format!("closed {peer}"),
        ForwardEvent::Failed(error) => format!("failed {error}"),
        ForwardEvent::Stopped => "stopped".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn resolve_target_prefers_saved_host_by_name() {
        let path = std::env::temp_dir().join(format!("openterm-cli-test-{}.redb", unique_suffix()));
        let store = WorkspaceStore::open(&path).unwrap();
        let mut host = HostProfile::new("prod", "10.0.0.8");
        host.username = Some("deploy".to_string());
        host.port = 2222;
        store.save_host(&host).unwrap();

        let resolved = resolve_target(&store, "prod", 22).unwrap();

        assert_eq!(resolved.display_target(), "deploy@10.0.0.8:2222");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn resolve_target_creates_direct_host_when_not_saved() {
        let path = std::env::temp_dir().join(format!("openterm-cli-test-{}.redb", unique_suffix()));
        let store = WorkspaceStore::open(&path).unwrap();

        let resolved = resolve_target(&store, "192.168.1.10", 2200).unwrap();

        assert_eq!(resolved.name, "192.168.1.10");
        assert_eq!(resolved.endpoint(), "192.168.1.10:2200");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn remote_file_kind_labels_are_stable() {
        assert_eq!(remote_file_kind_label(&RemoteFileKind::Directory), "dir");
        assert_eq!(remote_file_kind_label(&RemoteFileKind::File), "file");
        assert_eq!(remote_file_kind_label(&RemoteFileKind::Symlink), "link");
        assert_eq!(remote_file_kind_label(&RemoteFileKind::Other), "other");
    }

    #[test]
    fn forward_event_format_is_stable() {
        assert_eq!(
            format_forward_event(&ForwardEvent::Listening {
                bind_host: "127.0.0.1".to_string(),
                bind_port: 8080
            }),
            "listening 127.0.0.1:8080"
        );
    }

    #[test]
    fn cli_host_key_policy_respects_trust_flag() {
        assert_eq!(cli_host_key_policy(true), HostKeyPolicy::TrustAll);
        assert!(matches!(
            cli_host_key_policy(false),
            HostKeyPolicy::AcceptNew { .. }
        ));
    }

    #[test]
    fn cli_accepts_password_stdin_flag() {
        Cli::try_parse_from([
            "openterm",
            "exec",
            "82.157.57.178",
            "hostname",
            "--user",
            "ubuntu",
            "--password-stdin",
        ])
        .unwrap();
    }

    fn unique_suffix() -> String {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        format!("{}-{id}", std::process::id())
    }
}
