#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Bedrock Vault desktop shell. All world logic lives in `bedrock-vault-core`;
//! this file is the IPC surface plus DTOs for the UI.

use std::path::PathBuf;

use bedrock_vault_core::{guard, packs, scan, vault::Vault};
use chrono::Local;
use serde::Serialize;

#[derive(Serialize)]
struct WorldDto {
    /// Folder id for live worlds, vault id for library worlds.
    id: String,
    name: String,
    location: String,
    mode: String,
    version: String,
    last_played: Option<i64>,
    size_bytes: u64,
    /// `active` (in minecraftWorlds) or `library`.
    state: &'static str,
    /// Marketplace packs the world uses, resolved to names where possible.
    packs: Vec<String>,
    error: Option<String>,
}

#[derive(Serialize)]
struct OverviewDto {
    vault_root: String,
    game_running: Vec<String>,
    worlds: Vec<WorldDto>,
    /// Installed store content, grouped as "category: count".
    store_summary: Vec<String>,
}

fn vault_root() -> PathBuf {
    PathBuf::from(std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into())).join("BedrockVault")
}

fn stamp() -> String {
    Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// uuid -> display name for every installed pack, used to label world usage.
fn pack_index() -> std::collections::HashMap<String, String> {
    let mut index = std::collections::HashMap::new();
    for (_, cache) in packs::find_premium_caches() {
        if let Ok((found, _)) = packs::scan_premium_cache(&cache) {
            for p in found {
                index.insert(p.uuid, p.name);
            }
        }
    }
    index
}

fn world_pack_names(
    world_dir: &std::path::Path,
    index: &std::collections::HashMap<String, String>,
) -> Vec<String> {
    let (rp, bp) = packs::world_pack_refs(world_dir);
    let mut names: Vec<String> = rp
        .into_iter()
        .chain(bp)
        .map(|r| match index.get(&r.uuid) {
            Some(name) => name.clone(),
            None => "missing content".to_owned(),
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

#[tauri::command]
fn overview() -> Result<OverviewDto, String> {
    let index = pack_index();
    let mut worlds = Vec::new();

    for loc in scan::find_worlds_dirs().map_err(|e| e.to_string())? {
        if !loc.path.is_dir() {
            continue;
        }
        for w in scan::scan(&loc.path).map_err(|e| e.to_string())? {
            let dir = loc.path.join(&w.folder);
            let (mode, version, last_played, error) = match &w.meta {
                Ok(m) => (
                    m.game_mode_label().to_owned(),
                    m.version.clone().unwrap_or_else(|| "-".into()),
                    m.last_played,
                    None,
                ),
                Err(e) => ("-".into(), "-".into(), None, Some(format!("{e:#}"))),
            };
            worlds.push(WorldDto {
                id: w.folder.clone(),
                name: w.display_name(),
                location: loc.label.clone(),
                mode,
                version,
                last_played,
                size_bytes: w.size_bytes,
                state: "active",
                packs: world_pack_names(&dir, &index),
                error,
            });
        }
    }

    let vault = Vault::open(vault_root()).map_err(|e| e.to_string())?;
    for e in vault.list().map_err(|err| err.to_string())? {
        worlds.push(WorldDto {
            packs: world_pack_names(&e.world_dir, &index),
            id: e.id,
            name: e.name,
            location: "vault".into(),
            mode: e.game_mode.to_owned(),
            version: e.version.unwrap_or_else(|| "-".into()),
            last_played: e.last_played,
            size_bytes: e.size_bytes,
            state: "library",
            error: None,
        });
    }

    let mut store_summary = Vec::new();
    for (label, cache) in packs::find_premium_caches() {
        if let Ok((found, persona)) = packs::scan_premium_cache(&cache) {
            for (dir_name, title) in packs::CATEGORIES {
                let n = found.iter().filter(|p| p.category == *dir_name).count();
                if n > 0 {
                    store_summary.push(format!("{title}: {n}"));
                }
            }
            if persona > 0 {
                store_summary.push(format!("Persona items: {persona}"));
            }
            let _ = label;
        }
    }

    Ok(OverviewDto {
        vault_root: vault.root.display().to_string(),
        game_running: guard::game_status().running,
        worlds,
        store_summary,
    })
}

#[tauri::command]
fn archive(folder: String) -> Result<String, String> {
    let vault = Vault::open(vault_root()).map_err(|e| e.to_string())?;
    let locations = scan::find_worlds_dirs().map_err(|e| e.to_string())?;
    let live = locations
        .iter()
        .map(|l| l.path.join(&folder))
        .find(|p| p.join("level.dat").is_file())
        .ok_or_else(|| format!("no live world folder '{folder}'"))?;
    let entry = vault.archive(&live, &stamp()).map_err(|e| format!("{e:#}"))?;
    Ok(format!("Archived \"{}\" to the vault", entry.name))
}

#[tauri::command]
fn activate(id: String) -> Result<String, String> {
    let vault = Vault::open(vault_root()).map_err(|e| e.to_string())?;
    let entry = vault.entry(&id).map_err(|e| e.to_string())?;
    let locations = scan::find_worlds_dirs().map_err(|e| e.to_string())?;
    let target = locations
        .first()
        .ok_or_else(|| "no Minecraft install found".to_owned())?;
    vault
        .activate(&id, &target.path, &stamp())
        .map_err(|e| format!("{e:#}"))?;
    Ok(format!("\"{}\" is now in the in-game world list", entry.name))
}

#[tauri::command]
fn backup(folder: String) -> Result<String, String> {
    let vault = Vault::open(vault_root()).map_err(|e| e.to_string())?;
    let locations = scan::find_worlds_dirs().map_err(|e| e.to_string())?;
    let live = locations
        .iter()
        .map(|l| l.path.join(&folder))
        .find(|p| p.join("level.dat").is_file())
        .ok_or_else(|| format!("no live world folder '{folder}'"))?;
    guard::ensure_closed().map_err(|e| format!("{e:#}"))?;
    let out = vault
        .backup(&live, &folder, &stamp())
        .map_err(|e| format!("{e:#}"))?;
    Ok(format!("Backed up to {}", out.display()))
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![overview, archive, activate, backup])
        .run(tauri::generate_context!())
        .expect("error while running Bedrock Vault");
}
