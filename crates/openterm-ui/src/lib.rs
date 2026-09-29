use directories::ProjectDirs;
use std::path::PathBuf;

/// Where the workspace database lives (override with `OPENTERM_DB_PATH`).
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
