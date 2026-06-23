use directories::ProjectDirs;
use openterm_core::HostProfile;
use openterm_storage::WorkspaceStore;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct UiState {
    pub db_path: PathBuf,
    pub hosts: Vec<HostProfile>,
    pub selected: Option<usize>,
    pub status: String,
}

impl UiState {
    pub fn load() -> Self {
        let db_path = default_db_path();
        let hosts = WorkspaceStore::open(&db_path)
            .and_then(|store| store.list_hosts())
            .unwrap_or_default();

        Self {
            db_path,
            hosts,
            selected: None,
            status: "Offline ready. No account required.".to_string(),
        }
    }

    pub fn add_host(&mut self, profile: &HostProfile) -> Result<(), String> {
        let store = WorkspaceStore::open(&self.db_path).map_err(|error| error.to_string())?;
        store
            .save_host(profile)
            .map_err(|error| error.to_string())?;
        self.hosts = store.list_hosts().map_err(|error| error.to_string())?;
        self.status = "Host saved locally.".to_string();
        Ok(())
    }

    pub fn select_host(&mut self, index: usize) {
        self.selected = Some(index);
        if let Some(host) = self.hosts.get(index) {
            self.status = format!("Selected {}", host.display_target());
        }
    }

    pub fn render_text_shell(&self) -> String {
        let mut output = String::from("OpenTerm - Local SSH workbench\n");
        output.push_str("No login required. No cloud dependency. Your data stays local.\n\n");
        output.push_str("Hosts:\n");
        if self.hosts.is_empty() {
            output.push_str("  No hosts yet. Use openterm-cli add-host or import-ssh-config.\n");
        } else {
            for (index, host) in self.hosts.iter().enumerate() {
                output.push_str(&format!(
                    "  {}. {}  {}\n",
                    index + 1,
                    host.name,
                    host.display_target()
                ));
            }
        }
        output.push_str("\nWorkspace:\n");
        output.push_str(
            "  SSH transport and terminal renderer are isolated behind crate interfaces.\n",
        );
        output.push_str(&format!("\nStatus: {}\n", self.status));
        output
    }
}

pub fn default_db_path() -> PathBuf {
    if let Some(path) = std::env::var_os("OPENTERM_DB_PATH") {
        return PathBuf::from(path);
    }

    let dirs = ProjectDirs::from("dev", "OpenTerm", "OpenTerm")
        .expect("project directories should be available on desktop platforms");
    let data_dir = dirs.data_local_dir();
    let _ = std::fs::create_dir_all(data_dir);
    data_dir.join("openterm.redb")
}
