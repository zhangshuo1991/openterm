use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HostId(Uuid);

impl HostId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for HostId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for HostId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SecretId(Uuid);

impl SecretId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SecretId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SecretId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostProfile {
    pub id: HostId,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub group: Option<String>,
    pub tags: Vec<String>,
    pub auth: AuthRef,
    pub jump: Option<JumpConfig>,
    pub terminal: TerminalProfile,
    pub startup_command: Option<String>,
    pub sftp_root: Option<String>,
    #[serde(default)]
    pub last_connected_at: Option<String>,
}

impl HostProfile {
    pub fn new(name: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            id: HostId::new(),
            name: name.into(),
            host: host.into(),
            port: 22,
            username: None,
            group: None,
            tags: Vec::new(),
            auth: AuthRef::AgentOrDefault,
            jump: None,
            terminal: TerminalProfile::default(),
            startup_command: None,
            sftp_root: None,
            last_connected_at: None,
        }
    }

    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn display_target(&self) -> String {
        match &self.username {
            Some(username) if !username.is_empty() => {
                format!("{username}@{}:{}", self.host, self.port)
            }
            _ => self.endpoint(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthRef {
    AgentOrDefault,
    PasswordSecret(SecretId),
    PrivateKeyFile {
        path: String,
        passphrase: Option<SecretId>,
    },
    ManagedPrivateKey(SecretId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JumpConfig {
    pub host_id: HostId,
    pub username: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalProfile {
    pub theme: String,
    pub font_family: String,
    pub font_size: u16,
    pub scrollback_lines: usize,
}

impl Default for TerminalProfile {
    fn default() -> Self {
        Self {
            theme: "Dark".to_string(),
            font_family: "system-monospace".to_string(),
            font_size: 13,
            scrollback_lines: 10_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptedSecret {
    pub id: SecretId,
    pub version: u32,
    pub salt: Vec<u8>,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub kdf: KdfParams,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    pub algorithm: String,
    pub memory_cost_kib: u32,
    pub time_cost: u32,
    pub parallelism: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelProfile {
    pub id: Uuid,
    pub name: String,
    pub host_id: HostId,
    pub kind: TunnelKind,
    pub bind_addr: String,
    pub bind_port: u16,
    pub target_host: Option<String>,
    pub target_port: Option<u16>,
    pub auto_start: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TunnelKind {
    Local,
    Remote,
    DynamicSocks,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    pub id: Uuid,
    pub name: String,
    pub command: String,
    pub variables: Vec<String>,
    pub scope: SnippetScope,
    pub require_confirm: bool,
    pub danger_level: DangerLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnippetScope {
    Global,
    Group(String),
    Host(HostId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DangerLevel {
    Normal,
    Caution,
    Destructive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    Idle,
    Connecting,
    Authenticating,
    HostKeyVerificationRequired,
    Connected,
    Reconnecting,
    Closed,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppEvent {
    HostCreated(HostId),
    HostUpdated(HostId),
    HostDeleted(HostId),
    SessionConnecting(SessionId),
    SessionConnected(SessionId),
    SessionOutput(SessionId, Vec<u8>),
    SessionClosed(SessionId),
    VaultLocked,
    VaultUnlocked,
}

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("host name is required")]
    EmptyName,
    #[error("host address is required")]
    EmptyHost,
    #[error("port must be greater than zero")]
    InvalidPort,
}

pub fn validate_host(profile: &HostProfile) -> Result<(), ValidationError> {
    if profile.name.trim().is_empty() {
        return Err(ValidationError::EmptyName);
    }
    if profile.host.trim().is_empty() {
        return Err(ValidationError::EmptyHost);
    }
    if profile.port == 0 {
        return Err(ValidationError::InvalidPort);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_target_includes_username_when_present() {
        let mut host = HostProfile::new("prod", "10.0.0.5");
        host.username = Some("root".to_string());

        assert_eq!(host.display_target(), "root@10.0.0.5:22");
    }

    #[test]
    fn validation_rejects_blank_host() {
        let host = HostProfile::new("prod", " ");

        assert_eq!(
            validate_host(&host).unwrap_err().to_string(),
            "host address is required"
        );
    }

    #[test]
    fn host_profile_deserializes_without_last_connected_at() {
        let json = r#"{
            "id":"00000000-0000-0000-0000-000000000001",
            "name":"prod",
            "host":"10.0.0.5",
            "port":22,
            "username":"root",
            "group":null,
            "tags":[],
            "auth":"AgentOrDefault",
            "jump":null,
            "terminal":{"theme":"default","font_family":"monospace","font_size":14,"scrollback_lines":10000},
            "startup_command":null,
            "sftp_root":null
        }"#;

        let host: HostProfile = serde_json::from_str(json).unwrap();

        assert_eq!(host.display_target(), "root@10.0.0.5:22");
        assert_eq!(host.last_connected_at, None);
    }
}
