//! Device token storage (`crdt-sync.md` §10.1). macOS: the login keychain via
//! `security(1)`. Elsewhere: a 0600 JSON file under the config dir.
//! `ARBOS_RELAY_TOKEN` (+ `ARBOS_DEVICE_ID`) override both.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

const KEYCHAIN_SERVICE: &str = "sh.arbos.mac.relay";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceCredentials {
    pub relay_url: String,
    pub device_id: String,
    pub device_token: String,
}

fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("ARBOS_MAC_CONFIG") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config").join("arbos")
}

fn file_path(relay_url: &str) -> PathBuf {
    let slug: String = relay_url
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    config_dir().join(format!("mac-device-{slug}.json"))
}

pub fn load(relay_url: &str) -> Option<DeviceCredentials> {
    if let Ok(token) = std::env::var("ARBOS_RELAY_TOKEN") {
        if !token.trim().is_empty() {
            return Some(DeviceCredentials {
                relay_url: relay_url.to_string(),
                device_id: std::env::var("ARBOS_DEVICE_ID").unwrap_or_else(|_| "mac:env".into()),
                device_token: token.trim().to_string(),
            });
        }
    }
    if cfg!(target_os = "macos") {
        if let Some(c) = load_keychain(relay_url) {
            return Some(c);
        }
    }
    let bytes = std::fs::read(file_path(relay_url)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn store(creds: &DeviceCredentials) -> Result<()> {
    if cfg!(target_os = "macos") && store_keychain(creds).is_ok() {
        return Ok(());
    }
    let path = file_path(&creds.relay_url);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(creds)?;
    std::fs::write(&path, json).with_context(|| format!("write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn load_keychain(relay_url: &str) -> Option<DeviceCredentials> {
    let out = Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            relay_url,
            "-w",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let secret = String::from_utf8(out.stdout).ok()?;
    serde_json::from_str(secret.trim()).ok()
}

fn store_keychain(creds: &DeviceCredentials) -> Result<()> {
    let secret = serde_json::to_string(creds)?;
    let status = Command::new("security")
        .args([
            "add-generic-password",
            "-U",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            &creds.relay_url,
            "-w",
            &secret,
        ])
        .status()?;
    anyhow::ensure!(status.success(), "security add-generic-password failed");
    Ok(())
}

/// Where downloaded blobs go.
pub fn downloads_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let dl = PathBuf::from(&home).join("Downloads");
    if dl.is_dir() {
        dl
    } else {
        PathBuf::from(home)
    }
}
