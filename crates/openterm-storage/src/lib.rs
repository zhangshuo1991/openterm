use openterm_core::{validate_host, EncryptedSecret, HostId, HostProfile, SecretId};
use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};
use std::path::Path;

const HOSTS: TableDefinition<&str, &[u8]> = TableDefinition::new("host_profiles");
const SECRETS: TableDefinition<&str, &[u8]> = TableDefinition::new("encrypted_secrets");
const SETTINGS: TableDefinition<&str, &[u8]> = TableDefinition::new("app_settings");
const HISTORY: TableDefinition<u64, &[u8]> = TableDefinition::new("command_history");
const UI_SETTINGS_KEY: &str = "ui";
const MAX_HISTORY_ROWS: u64 = 5000;

/// One persisted command-history entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Unix timestamp in milliseconds.
    pub ts_ms: u64,
    /// "user@host" label for the session this command came from.
    pub host: String,
    pub cmd: String,
    /// First ~5 KB of the command's output (ANSI-stripped). Empty for old entries.
    #[serde(default)]
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiSettings {
    pub theme_mode: String,
    pub terminal_font_size: u16,
    #[serde(default)]
    pub color_scheme: String,
    /// Default username pre-filled for new sessions (empty = use $USER).
    #[serde(default)]
    pub default_user: String,
    /// Default SSH port pre-filled for new sessions.
    #[serde(default = "default_port")]
    pub default_port: u16,
}

fn default_port() -> u16 {
    22
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            theme_mode: "dark".to_string(),
            terminal_font_size: 14,
            color_scheme: "DarkTeal".to_string(),
            default_user: String::new(),
            default_port: 22,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error(transparent)]
    Database(#[from] redb::Error),
    #[error(transparent)]
    DatabaseOpen(#[from] redb::DatabaseError),
    #[error(transparent)]
    Table(#[from] redb::TableError),
    #[error(transparent)]
    Transaction(#[from] redb::TransactionError),
    #[error(transparent)]
    Commit(#[from] redb::CommitError),
    #[error(transparent)]
    Storage(#[from] redb::StorageError),
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
    #[error(transparent)]
    Validation(#[from] openterm_core::ValidationError),
}

pub struct WorkspaceStore {
    db: Database,
}

impl WorkspaceStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        Ok(Self {
            db: Database::create(path)?,
        })
    }

    pub fn save_host(&self, profile: &HostProfile) -> Result<(), StorageError> {
        validate_host(profile)?;
        let bytes = serde_json::to_vec(profile)?;
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(HOSTS)?;
            table.insert(profile.id.to_string().as_str(), bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    pub fn list_hosts(&self) -> Result<Vec<HostProfile>, StorageError> {
        let txn = self.db.begin_read()?;
        let Ok(table) = txn.open_table(HOSTS) else {
            return Ok(Vec::new());
        };
        let mut hosts = Vec::new();
        for row in table.iter()? {
            let (_, value) = row?;
            hosts.push(serde_json::from_slice(value.value())?);
        }
        hosts.sort_by(|a: &HostProfile, b| a.name.cmp(&b.name));
        Ok(hosts)
    }

    pub fn get_host(&self, id: HostId) -> Result<Option<HostProfile>, StorageError> {
        let txn = self.db.begin_read()?;
        let Ok(table) = txn.open_table(HOSTS) else {
            return Ok(None);
        };
        let Some(value) = table.get(id.to_string().as_str())? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_slice(value.value())?))
    }

    pub fn delete_host(&self, id: HostId) -> Result<bool, StorageError> {
        let txn = self.db.begin_write()?;
        let removed = {
            let mut table = txn.open_table(HOSTS)?;
            let removed = table.remove(id.to_string().as_str())?.is_some();
            removed
        };
        txn.commit()?;
        Ok(removed)
    }

    pub fn save_secret(&self, secret: &EncryptedSecret) -> Result<(), StorageError> {
        let bytes = serde_json::to_vec(secret)?;
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(SECRETS)?;
            table.insert(secret.id.to_string().as_str(), bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    pub fn get_secret(&self, id: SecretId) -> Result<Option<EncryptedSecret>, StorageError> {
        let txn = self.db.begin_read()?;
        let Ok(table) = txn.open_table(SECRETS) else {
            return Ok(None);
        };
        let Some(value) = table.get(id.to_string().as_str())? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_slice(value.value())?))
    }

    pub fn save_ui_settings(&self, settings: &UiSettings) -> Result<(), StorageError> {
        let bytes = serde_json::to_vec(settings)?;
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(SETTINGS)?;
            table.insert(UI_SETTINGS_KEY, bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    pub fn get_ui_settings(&self) -> Result<Option<UiSettings>, StorageError> {
        let txn = self.db.begin_read()?;
        let Ok(table) = txn.open_table(SETTINGS) else {
            return Ok(None);
        };
        let Some(value) = table.get(UI_SETTINGS_KEY)? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_slice(value.value())?))
    }

    /// Append one command to persistent history, pruning oldest if over the cap.
    pub fn append_history(&self, entry: &HistoryEntry) -> Result<(), StorageError> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(HISTORY)?;
            // Key = next monotonic id (last key + 1, or 1 if empty).
            let next_key = table
                .iter()?
                .next_back()
                .transpose()?
                .map(|(k, _)| k.value() + 1)
                .unwrap_or(1);
            let bytes = serde_json::to_vec(entry)?;
            table.insert(next_key, bytes.as_slice())?;
            // Prune oldest if over cap.
            let len = table.len()?;
            if len > MAX_HISTORY_ROWS {
                let to_drop: Vec<u64> = table
                    .iter()?
                    .take((len - MAX_HISTORY_ROWS) as usize)
                    .filter_map(|r| r.ok().map(|(k, _)| k.value()))
                    .collect();
                for k in to_drop {
                    table.remove(k)?;
                }
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Load all history entries, newest-first.
    pub fn load_history(&self) -> Result<Vec<HistoryEntry>, StorageError> {
        let txn = self.db.begin_read()?;
        let Ok(table) = txn.open_table(HISTORY) else {
            return Ok(Vec::new());
        };
        let mut entries: Vec<HistoryEntry> = table
            .iter()?
            .filter_map(|r| {
                let (_, v) = r.ok()?;
                serde_json::from_slice(v.value()).ok()
            })
            .collect();
        entries.reverse(); // newest-first
        Ok(entries)
    }

    /// Backfill the output field of the most recent history entry.
    pub fn update_last_history_output(&self, output: &str) -> Result<(), StorageError> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(HISTORY)?;
            // Collect key + updated bytes before any mutable borrow.
            let update = table
                .iter()?
                .next_back()
                .transpose()?
                .map(|(k, v)| -> Result<_, StorageError> {
                    let key = k.value();
                    let mut entry: HistoryEntry = serde_json::from_slice(v.value())?;
                    entry.output = output.to_string();
                    Ok((key, serde_json::to_vec(&entry)?))
                })
                .transpose()?;
            if let Some((key, bytes)) = update {
                table.insert(key, bytes.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Delete all history entries.
    pub fn clear_history(&self) -> Result<(), StorageError> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(HISTORY)?;
            // drain all keys
            let keys: Vec<u64> = table
                .iter()?
                .filter_map(|r| r.ok().map(|(k, _)| k.value()))
                .collect();
            for k in keys {
                table.remove(k)?;
            }
        }
        txn.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn saves_and_lists_hosts() {
        let path = std::env::temp_dir().join(format!("openterm-test-{}.redb", uuid_like()));
        let store = WorkspaceStore::open(&path).unwrap();
        let mut host = HostProfile::new("local", "127.0.0.1");
        host.username = Some("me".to_string());

        store.save_host(&host).unwrap();

        let hosts = store.list_hosts().unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].display_target(), "me@127.0.0.1:22");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn saves_and_loads_ui_settings() {
        let path = std::env::temp_dir().join(format!("openterm-settings-{}.redb", uuid_like()));
        let store = WorkspaceStore::open(&path).unwrap();
        let settings = UiSettings {
            theme_mode: "light".to_string(),
            terminal_font_size: 18,
            default_user: "me".to_string(),
            default_port: 2222,
            color_scheme: "DarkTeal".to_string(),
        };

        assert_eq!(store.get_ui_settings().unwrap(), None);
        store.save_ui_settings(&settings).unwrap();

        assert_eq!(store.get_ui_settings().unwrap(), Some(settings));

        let _ = fs::remove_file(path);
    }

    fn uuid_like() -> String {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string()
    }
}
