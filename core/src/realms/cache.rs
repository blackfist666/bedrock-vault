//! Local token cache: `%APPDATA%\BedrockVault\auth.json`.
//!
//! Tokens never leave this machine and are never logged. Signing out deletes
//! the file outright.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::auth::{MicrosoftTokens, XstsToken};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Session {
    pub microsoft: Option<MicrosoftTokens>,
    pub realms: Option<XstsToken>,
}

pub fn path() -> Result<PathBuf> {
    let roaming = std::env::var("APPDATA").context("APPDATA is not set")?;
    Ok(PathBuf::from(roaming).join("BedrockVault").join("auth.json"))
}

pub fn load() -> Session {
    path()
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(session: &Session) -> Result<()> {
    let path = path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_string_pretty(session)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Forget the signed-in account entirely.
pub fn clear() -> Result<()> {
    let path = path()?;
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}

pub fn is_signed_in() -> bool {
    load().microsoft.is_some()
}
