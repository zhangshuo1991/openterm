use openterm_core::{AuthRef, HostProfile};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to encode config")]
    Encode(#[from] toml::ser::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostExport {
    pub hosts: Vec<HostProfile>,
}

pub fn export_hosts_toml(hosts: &[HostProfile]) -> Result<String, ConfigError> {
    Ok(toml::to_string_pretty(&HostExport {
        hosts: hosts.to_vec(),
    })?)
}

pub fn import_openssh_config(input: &str) -> Vec<HostProfile> {
    let mut hosts = Vec::new();
    let mut current: Option<OpenSshHost> = None;

    for raw_line in input.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = split_directive(line) else {
            continue;
        };

        if key.eq_ignore_ascii_case("Host") {
            if let Some(host) = current.take().and_then(OpenSshHost::into_profile) {
                hosts.push(host);
            }
            current = Some(OpenSshHost {
                alias: value.to_string(),
                ..OpenSshHost::default()
            });
            continue;
        }

        let Some(host) = current.as_mut() else {
            continue;
        };

        match key.to_ascii_lowercase().as_str() {
            "hostname" => host.hostname = Some(value.to_string()),
            "user" => host.user = Some(value.to_string()),
            "port" => host.port = value.parse().ok(),
            "identityfile" => host.identity_file = Some(expand_home(value)),
            "proxyjump" => host.proxy_jump = Some(value.to_string()),
            _ => {}
        }
    }

    if let Some(host) = current.and_then(OpenSshHost::into_profile) {
        hosts.push(host);
    }

    hosts
}

fn split_directive(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.splitn(2, char::is_whitespace);
    let key = parts.next()?.trim();
    let value = parts.next()?.trim();
    if key.is_empty() || value.is_empty() {
        None
    } else {
        Some((key, value.trim_matches('"')))
    }
}

fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return format!("{}/{}", home.to_string_lossy(), rest);
        }
    }
    path.to_string()
}

#[derive(Debug, Default)]
struct OpenSshHost {
    alias: String,
    hostname: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    identity_file: Option<String>,
    proxy_jump: Option<String>,
}

impl OpenSshHost {
    fn into_profile(self) -> Option<HostProfile> {
        if self.alias == "*" || self.alias.contains('*') || self.alias.contains('?') {
            return None;
        }

        let mut profile = HostProfile::new(self.alias.clone(), self.hostname.unwrap_or(self.alias));
        profile.username = self.user;
        profile.port = self.port.unwrap_or(22);
        if let Some(path) = self.identity_file {
            profile.auth = AuthRef::PrivateKeyFile {
                path,
                passphrase: None,
            };
        }
        if let Some(jump) = self.proxy_jump {
            profile.tags.push(format!("proxyjump:{jump}"));
        }
        Some(profile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_basic_openssh_hosts() {
        let hosts = import_openssh_config(
            r#"
            Host *
              ServerAliveInterval 30

            Host prod-api
              HostName 10.20.1.15
              User root
              Port 2222
              IdentityFile ~/.ssh/prod
            "#,
        );

        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].name, "prod-api");
        assert_eq!(hosts[0].display_target(), "root@10.20.1.15:2222");
    }
}
