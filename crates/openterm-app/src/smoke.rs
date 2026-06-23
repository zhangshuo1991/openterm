//! Optional smoke-test automation, driven entirely by environment variables so
//! it never affects normal use. Lets an automated run drive the GUI to connect
//! to a real server and record lifecycle milestones to a status file, which the
//! verification scripts assert on.
//!
//! Active only when `OPENTERM_SMOKE_CONNECT=1`.

use std::io::Write;
use std::path::PathBuf;

use crate::session::{AuthMode, SessionConfig};

pub struct SmokeConfig {
    pub status_path: Option<PathBuf>,
}

impl SmokeConfig {
    /// Read smoke settings from the environment. Returns `None` when smoke mode
    /// is off.
    pub fn from_env() -> Option<(SessionConfig, SmokeConfig)> {
        if std::env::var_os("OPENTERM_SMOKE_CONNECT").is_none() {
            return None;
        }
        let host = std::env::var("OPENTERM_SMOKE_HOST").ok()?;
        let user = std::env::var("OPENTERM_SMOKE_USER").unwrap_or_else(|_| "root".to_string());
        let password = std::env::var("OPENTERM_TEST_PASSWORD").unwrap_or_default();

        let mut config = SessionConfig::blank(user, crate::default_key_path());
        config.name = "smoke".to_string();
        config.host = host;
        config.port = std::env::var("OPENTERM_SMOKE_PORT").unwrap_or_else(|_| "22".to_string());
        config.auth = AuthMode::Password;
        config.password = password;

        let smoke = SmokeConfig {
            status_path: std::env::var_os("OPENTERM_SMOKE_STATUS").map(PathBuf::from),
        };
        Some((config, smoke))
    }
}

/// Append a milestone line to the smoke status file (best-effort).
pub fn record(path: &Option<PathBuf>, milestone: &str) {
    if let Some(path) = path {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{milestone}");
        }
    }
}
