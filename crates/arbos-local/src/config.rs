//! Local config: relay URL and the device token from pairing (same D35 pattern as `arbosctl`,
//! kept in its own file so revoking the helper never touches the CLI's token).

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(skip)]
    pub path: PathBuf,
    #[serde(default = "default_relay")]
    pub relay_url: String,
    #[serde(default)]
    pub device_token: String,
    #[serde(default)]
    pub device_id: String,
}

fn default_relay() -> String {
    "http://127.0.0.1:7700".to_owned()
}

impl Config {
    pub fn load(path: Option<PathBuf>) -> Result<Self> {
        let path = path.unwrap_or_else(|| {
            let base = std::env::var("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
                        .join(".config")
                });
            base.join("arbos").join("arbos-local.json")
        });
        let mut cfg: Config = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s)?,
            Err(_) => Config {
                path: path.clone(),
                relay_url: default_relay(),
                device_token: String::new(),
                device_id: String::new(),
            },
        };
        cfg.path = path;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        if let Some(p) = self.path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&self.path, serde_json::to_string_pretty(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    pub fn require_paired(&self) -> Result<()> {
        if self.device_token.is_empty() || self.device_id.is_empty() {
            return Err(anyhow!(
                "not paired: run `arbos-local pair <code> --relay <url>` first"
            ));
        }
        Ok(())
    }

    /// `ws(s)://…/local/{deviceId}?token=…` for the relay's helper socket.
    pub fn ws_url(&self) -> String {
        let base = self.relay_url.trim_end_matches('/');
        let ws = if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            base.to_owned()
        };
        format!("{ws}/local/{}?token={}", self.device_id, self.device_token)
    }
}
