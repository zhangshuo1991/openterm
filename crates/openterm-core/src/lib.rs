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
            "auth":"AgentOrDefault"
        }"#;

        let host: HostProfile = serde_json::from_str(json).unwrap();

        assert_eq!(host.display_target(), "root@10.0.0.5:22");
        assert_eq!(host.last_connected_at, None);
    }
}
