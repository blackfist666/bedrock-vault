//! Locate and scan the live `minecraftWorlds` directory.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::level_dat::{self, WorldMeta};

const PACKAGE_IDS: &[&str] = &[
    "Microsoft.MinecraftUWP_8wekyb3d8bbwe",        // release
    "Microsoft.MinecraftWindowsBeta_8wekyb3d8bbwe", // preview
];

/// Resolve the live minecraftWorlds directory, preferring the release package.
///
/// The folder does not exist until the game first creates or downloads a local
/// world, so an installed game with a `com.mojang` tree but no `minecraftWorlds`
/// resolves to the path the game *will* use (which may not exist yet).
pub fn find_worlds_dir() -> Result<PathBuf> {
    let local = std::env::var("LOCALAPPDATA")
        .map_err(|_| anyhow::anyhow!("LOCALAPPDATA is not set"))?;
    for pkg in PACKAGE_IDS {
        let mojang = Path::new(&local)
            .join("Packages")
            .join(pkg)
            .join(r"LocalState\games\com.mojang");
        if mojang.is_dir() {
            return Ok(mojang.join("minecraftWorlds"));
        }
    }
    bail!("no Bedrock com.mojang directory found under LOCALAPPDATA (is Minecraft installed?)");
}

pub struct ScannedWorld {
    pub folder: String,
    pub meta: Result<WorldMeta>,
    pub levelname_txt: Option<String>,
    pub size_bytes: u64,
    #[allow(dead_code)] // surfaced in the Tier 1 UI, not in the spike table
    pub has_icon: bool,
}

impl ScannedWorld {
    /// Display name: level.dat wins, levelname.txt is the fallback.
    pub fn display_name(&self) -> String {
        if let Ok(meta) = &self.meta {
            if let Some(n) = &meta.name {
                return n.clone();
            }
        }
        self.levelname_txt.clone().unwrap_or_else(|| "<unnamed>".into())
    }
}

pub fn scan(dir: &Path) -> Result<Vec<ScannedWorld>> {
    let mut worlds = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        // Skip folders that aren't worlds at all (no level.dat anywhere).
        let level_dat = path.join("level.dat");
        if !level_dat.is_file() {
            continue;
        }
        let meta = fs::read(&level_dat)
            .map_err(anyhow::Error::from)
            .and_then(|data| level_dat::parse(&data));
        let levelname_txt = fs::read_to_string(path.join("levelname.txt"))
            .ok()
            .map(|s| s.trim().to_owned());
        worlds.push(ScannedWorld {
            folder: entry.file_name().to_string_lossy().into_owned(),
            meta,
            levelname_txt,
            size_bytes: dir_size(&path),
            has_icon: path.join("world_icon.jpeg").is_file(),
        });
    }
    // Most recently played first; unparsable worlds sink to the bottom.
    worlds.sort_by_key(|w| {
        std::cmp::Reverse(w.meta.as_ref().ok().and_then(|m| m.last_played).unwrap_or(i64::MIN))
    });
    Ok(worlds)
}

pub fn dir_size(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}
