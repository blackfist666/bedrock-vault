//! App configuration, stored outside the vault.
//!
//! The vault's own `settings.json` lives *inside* the vault, so it cannot hold
//! the vault's location. This file answers "where is the vault?" and lives in
//! `%APPDATA%\BedrockVault\config.json`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Where the vault lives; `None` means the default location.
    pub vault_root: Option<PathBuf>,
}

/// `%APPDATA%\BedrockVault\config.json`
pub fn config_path() -> Result<PathBuf> {
    let roaming = std::env::var("APPDATA").context("APPDATA is not set")?;
    Ok(Path::new(&roaming).join("BedrockVault").join("config.json"))
}

/// `%USERPROFILE%\BedrockVault`
pub fn default_vault_root() -> PathBuf {
    let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
    Path::new(&home).join("BedrockVault")
}

pub fn load() -> Config {
    config_path()
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(config: &Config) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_string_pretty(config)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// The vault location in force: the configured one, else the default.
pub fn vault_root() -> PathBuf {
    load().vault_root.unwrap_or_else(default_vault_root)
}

/// Point the app at a different vault folder.
///
/// Moving existing data is the caller's job — see
/// [`crate::vault::move_vault`] — because it needs progress reporting.
pub fn set_vault_root(root: &Path) -> Result<()> {
    let mut config = load();
    config.vault_root = Some(root.to_path_buf());
    save(&config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let config = Config { vault_root: Some(PathBuf::from(r"D:\Games\Vault")) };
        let json = serde_json::to_string(&config).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.vault_root, config.vault_root);
    }

    #[test]
    fn missing_or_broken_config_falls_back_to_default() {
        let broken: Config = serde_json::from_str("{}").unwrap();
        assert!(broken.vault_root.is_none());
        assert!(default_vault_root().ends_with("BedrockVault"));
    }
}
