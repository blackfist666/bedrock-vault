//! Locate and scan the live `minecraftWorlds` directory.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::level_dat::{self, WorldMeta};

const PACKAGE_IDS: &[&str] = &[
    "Microsoft.MinecraftUWP_8wekyb3d8bbwe",        // release
    "Microsoft.MinecraftWindowsBeta_8wekyb3d8bbwe", // preview
];

/// A live world location the game may read from.
pub struct WorldsLocation {
    /// Human label, e.g. `GDK user 168…474` or `UWP`.
    pub label: String,
    pub path: PathBuf,
}

/// Find every live minecraftWorlds directory on this machine.
///
/// Two storage layouts exist:
/// - **GDK** (current, new launcher): `%APPDATA%\Minecraft Bedrock\Users\<xuid-or-Shared>\games\com.mojang\minecraftWorlds` — one folder per signed-in profile
/// - **UWP** (legacy Store package): `%LOCALAPPDATA%\Packages\<pkg>\LocalState\games\com.mojang\minecraftWorlds` — may be a junction to another drive if the app was moved
///
/// A `com.mojang` tree without `minecraftWorlds` means the install exists but no
/// local world has been created yet; such locations are still returned (the game
/// will create the folder) and scan as empty.
pub fn find_worlds_dirs() -> Result<Vec<WorldsLocation>> {
    let mut found = Vec::new();

    if let Ok(roaming) = std::env::var("APPDATA") {
        let users = Path::new(&roaming).join(r"Minecraft Bedrock\Users");
        if users.is_dir() {
            let mut profiles: Vec<_> = fs::read_dir(&users)?
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .collect();
            profiles.sort_by_key(|e| e.file_name());
            for profile in profiles {
                let mojang = profile.path().join(r"games\com.mojang");
                if mojang.is_dir() {
                    found.push(WorldsLocation {
                        label: format!("GDK {}", profile.file_name().to_string_lossy()),
                        path: mojang.join("minecraftWorlds"),
                    });
                }
            }
        }
    }

    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        for pkg in PACKAGE_IDS {
            let mojang = Path::new(&local)
                .join("Packages")
                .join(pkg)
                .join(r"LocalState\games\com.mojang");
            if mojang.is_dir() {
                found.push(WorldsLocation {
                    label: if pkg.contains("Beta") { "UWP preview".into() } else { "UWP".into() },
                    path: mojang.join("minecraftWorlds"),
                });
            }
        }
    }

    if found.is_empty() {
        bail!("no Bedrock com.mojang directory found (is Minecraft installed?)");
    }
    Ok(found)
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
