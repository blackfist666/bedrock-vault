#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Bedrock Vault desktop shell.
//!
//! The UI is three sections — **Live** (what Minecraft sees), **Vault** (every
//! world she owns) and **Backups** (every snapshot) — so all world logic lives
//! in `bedrock-vault-core` and this file only shapes it for the screen.

use std::collections::HashMap;
use std::path::PathBuf;

use bedrock_vault_core::{
    config, guard, packs, realms, scan,
    vault::{self, Protection, Vault},
};
use chrono::Local;
use serde::Serialize;
use tauri::Emitter;

/// Run blocking file work off the UI thread.
///
/// Tauri drives synchronous commands on the main thread, so a multi-gigabyte
/// world copy would freeze the window until it finished. Every command that
/// touches the disk goes through here.
async fn blocking<T, F>(work: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| format!("background task failed: {e}"))?
}

#[derive(Clone, Serialize)]
struct ProgressDto {
    done: usize,
    total: usize,
    current: String,
}

#[derive(Serialize)]
struct LiveWorldDto {
    /// `minecraftWorlds` folder id.
    folder: String,
    name: String,
    mode: String,
    version: String,
    last_played: Option<i64>,
    size_bytes: u64,
    /// True when an up-to-date copy of this world is in the vault.
    saved: bool,
    /// Human phrase for the saved state, e.g. "Saved to the vault".
    saved_label: String,
    packs: Vec<String>,
    missing_packs: usize,
    icon: Option<String>,
    error: Option<String>,
}

#[derive(Serialize)]
struct VaultWorldDto {
    id: String,
    name: String,
    mode: String,
    version: String,
    last_played: Option<i64>,
    size_bytes: u64,
    /// True when this world is also in Minecraft right now.
    in_game: bool,
    packs: Vec<String>,
    missing_packs: usize,
    backup_count: usize,
    icon: Option<String>,
}

#[derive(Serialize)]
struct BackupDto {
    label: String,
    size_bytes: u64,
    path: String,
}

#[derive(Serialize)]
struct BackupGroupDto {
    name: String,
    total_bytes: u64,
    backups: Vec<BackupDto>,
}

#[derive(Serialize)]
struct OverviewDto {
    vault_root: String,
    game_running: Vec<String>,
    live: Vec<LiveWorldDto>,
    vault: Vec<VaultWorldDto>,
    backups: Vec<BackupGroupDto>,
    /// Live worlds with no up-to-date vault copy.
    unsaved: usize,
    live_bytes: u64,
    vault_bytes: u64,
    backup_bytes: u64,
}

fn vault_root() -> PathBuf {
    config::vault_root()
}

fn stamp() -> String {
    Local::now().format("%Y%m%d-%H%M%S").to_string()
}

fn open_vault() -> Result<Vault, String> {
    Vault::open(vault_root()).map_err(|e| format!("{e:#}"))
}

/// uuid -> display name for every installed pack, used to label world usage.
fn pack_index() -> HashMap<String, String> {
    let mut index = HashMap::new();
    for (_, cache) in packs::find_premium_caches() {
        if let Ok((found, _)) = packs::scan_premium_cache(&cache) {
            for p in found {
                index.insert(p.uuid, p.name);
            }
        }
    }
    index
}

/// Pack names for a world, plus a count of ones not installed on this machine.
fn world_packs(world_dir: &std::path::Path, index: &HashMap<String, String>) -> (Vec<String>, usize) {
    let (rp, bp) = packs::world_pack_refs(world_dir);
    let mut names = Vec::new();
    let mut missing = 0;
    for r in rp.into_iter().chain(bp) {
        match index.get(&r.uuid) {
            Some(name) => names.push(name.clone()),
            None => missing += 1,
        }
    }
    names.sort();
    names.dedup();
    (names, missing)
}

/// A world's `world_icon.jpeg` as a data URI, for the thumbnail in the list.
///
/// Inlined rather than served from disk because the webview's content policy
/// blocks arbitrary local files. Oversized icons are skipped rather than
/// bloating every refresh.
fn world_icon(world_dir: &std::path::Path) -> Option<String> {
    use base64::Engine;

    const MAX_ICON_BYTES: u64 = 512 * 1024;
    let path = world_dir.join("world_icon.jpeg");
    if std::fs::metadata(&path).ok()?.len() > MAX_ICON_BYTES {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    Some(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// `20260810-102406` -> `10 Aug 2026, 10:24`; deletion snapshots say so.
fn pretty_stamp(stamp: &str) -> String {
    let (core, suffix) = match stamp.strip_suffix("-deleted") {
        Some(rest) => (rest, " · kept when deleted"),
        None => (stamp, ""),
    };
    match chrono::NaiveDateTime::parse_from_str(core, "%Y%m%d-%H%M%S") {
        Ok(dt) => format!("{}{suffix}", dt.format("%d %b %Y, %H:%M")),
        Err(_) => format!("{core}{suffix}"),
    }
}

#[tauri::command]
async fn overview() -> Result<OverviewDto, String> {
    blocking(build_overview).await
}

/// Cheap poll of just the process guard.
///
/// The UI checks this on a timer so the "Minecraft is open" warning appears
/// when the game is started *after* the window was opened — the full overview
/// walks the worlds and is far too heavy to run every few seconds.
#[tauri::command]
async fn game_status() -> Result<Vec<String>, String> {
    blocking(|| Ok(guard::game_status().running)).await
}

fn build_overview() -> Result<OverviewDto, String> {
    let index = pack_index();
    let vault = open_vault()?;
    let library = vault.list().map_err(|e| format!("{e:#}"))?;

    let mut live = Vec::new();
    let mut live_folders: Vec<String> = Vec::new();
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
            // Compared against the library listed once above, not re-read per
            // world: that turned a refresh into an O(n²) disk crawl.
            let entry = library.iter().find(|e| e.origin_folder.as_deref() == Some(&w.folder));
            let protection = Protection::compare(entry, last_played);
            let saved = protection == Protection::Protected;
            let (names, missing) = world_packs(&dir, &index);
            live_folders.push(w.folder.clone());
            live.push(LiveWorldDto {
                folder: w.folder.clone(),
                name: w.display_name(),
                mode,
                version,
                last_played,
                size_bytes: w.size_bytes,
                saved,
                saved_label: match protection {
                    Protection::Protected => "Saved in the vault".into(),
                    Protection::Stale => "Played since it was last saved".into(),
                    Protection::None => "Not in the vault yet".into(),
                },
                packs: names,
                missing_packs: missing,
                icon: world_icon(&dir),
                error,
            });
        }
    }

    let vault_worlds: Vec<VaultWorldDto> = library
        .iter()
        .map(|e| {
            let (names, missing) = world_packs(&e.world_dir, &index);
            VaultWorldDto {
                id: e.id.clone(),
                name: e.name.clone(),
                mode: e.game_mode.to_owned(),
                version: e.version.clone().unwrap_or_else(|| "-".into()),
                last_played: e.last_played,
                size_bytes: e.size_bytes,
                in_game: e
                    .origin_folder
                    .as_ref()
                    .is_some_and(|f| live_folders.contains(f)),
                packs: names,
                missing_packs: missing,
                backup_count: vault.snapshots(e).len(),
                icon: world_icon(&e.world_dir),
            }
        })
        .collect();

    let backups: Vec<BackupGroupDto> = vault
        .all_backups(&library)
        .into_iter()
        .map(|g| BackupGroupDto {
            name: g.name,
            total_bytes: g.snapshots.iter().map(|s| s.size_bytes).sum(),
            backups: g
                .snapshots
                .into_iter()
                .map(|s| BackupDto {
                    label: pretty_stamp(&s.stamp),
                    size_bytes: s.size_bytes,
                    path: s.path.display().to_string(),
                })
                .collect(),
        })
        .collect();

    Ok(OverviewDto {
        vault_root: vault.root.display().to_string(),
        game_running: guard::game_status().running,
        unsaved: live.iter().filter(|w| !w.saved).count(),
        live_bytes: live.iter().map(|w| w.size_bytes).sum(),
        vault_bytes: vault_worlds.iter().map(|w| w.size_bytes).sum(),
        backup_bytes: backups.iter().map(|g| g.total_bytes).sum(),
        live,
        vault: vault_worlds,
        backups,
    })
}

/// Resolve a live world folder id to its path.
fn live_path(folder: &str) -> Result<PathBuf, String> {
    scan::find_worlds_dirs()
        .map_err(|e| e.to_string())?
        .iter()
        .map(|l| l.path.join(folder))
        .find(|p| p.join("level.dat").is_file())
        .ok_or_else(|| format!("no live world folder '{folder}'"))
}

/// Copy a live world into the vault, keeping it in the game.
#[tauri::command]
async fn save_to_vault(folder: String) -> Result<String, String> {
    blocking(move || {
        let vault = open_vault()?;
        let live = live_path(&folder)?;
        let entry = vault
            .protect(&live, &folder, &stamp())
            .map_err(|e| format!("{e:#}"))?;
        Ok(format!("\"{}\" is saved in the vault", entry.name))
    })
    .await
}

/// Save every live world that is not already up to date in the vault.
#[tauri::command]
async fn save_all(app: tauri::AppHandle) -> Result<String, String> {
    blocking(move || {
        let vault = open_vault()?;
        guard::ensure_closed().map_err(|e| format!("{e:#}"))?;

        // Collect the work first so progress can report a real total.
        let mut todo = Vec::new();
        for loc in scan::find_worlds_dirs().map_err(|e| e.to_string())? {
            if !loc.path.is_dir() {
                continue;
            }
            for w in scan::scan(&loc.path).map_err(|e| e.to_string())? {
                let last_played = w.meta.as_ref().ok().and_then(|m| m.last_played);
                if vault.protection(&w.folder, last_played).unwrap_or(Protection::None)
                    == Protection::Protected
                {
                    continue;
                }
                todo.push((loc.path.join(&w.folder), w.folder.clone(), w.display_name()));
            }
        }

        let total = todo.len();
        let mut done = 0;
        let mut failed = Vec::new();
        for (path, folder, name) in todo {
            let _ = app.emit("progress", ProgressDto { done, total, current: name.clone() });
            match vault.protect(&path, &folder, &stamp()) {
                Ok(_) => done += 1,
                Err(e) => failed.push(format!("{name}: {e:#}")),
            }
        }
        let _ = app.emit("progress", ProgressDto { done, total, current: String::new() });

        if !failed.is_empty() {
            return Err(format!(
                "Saved {done}, but {} failed — {}",
                failed.len(),
                failed.join("; ")
            ));
        }
        Ok(match done {
            0 => "Everything was already saved".to_owned(),
            1 => "Saved 1 world to the vault".to_owned(),
            n => format!("Saved {n} worlds to the vault"),
        })
    })
    .await
}

/// Save a live world and remove it from the in-game list.
#[tauri::command]
async fn put_away(folder: String) -> Result<String, String> {
    blocking(move || {
        let vault = open_vault()?;
        let live = live_path(&folder)?;
        let entry = vault
            .archive(&live, &folder, &stamp())
            .map_err(|e| format!("{e:#}"))?;
        Ok(format!(
            "\"{}\" is in the vault and out of Minecraft's list",
            entry.name
        ))
    })
    .await
}

/// Copy a vault world into Minecraft so it shows up in the game.
#[tauri::command]
async fn play(id: String) -> Result<String, String> {
    blocking(move || {
        let vault = open_vault()?;
        let entry = vault.entry(&id).map_err(|e| format!("{e:#}"))?;
        let locations = scan::find_worlds_dirs().map_err(|e| e.to_string())?;
        let target = locations.first().ok_or_else(|| "no Minecraft install found".to_owned())?;
        vault
            .activate(&id, &target.path, &stamp())
            .map_err(|e| format!("{e:#}"))?;
        Ok(format!("\"{}\" is ready to play in Minecraft", entry.name))
    })
    .await
}

/// Take a fresh snapshot of a vault world.
#[tauri::command]
async fn back_up(id: String) -> Result<String, String> {
    blocking(move || {
        let vault = open_vault()?;
        let entry = vault.entry(&id).map_err(|e| format!("{e:#}"))?;
        vault
            .snapshot(&id, &entry.world_dir, &stamp())
            .map_err(|e| format!("{e:#}"))?;
        Ok(format!("Backed up \"{}\"", entry.name))
    })
    .await
}

#[tauri::command]
async fn delete(id: String) -> Result<String, String> {
    blocking(move || {
        let vault = open_vault()?;
        let entry = vault.entry(&id).map_err(|e| format!("{e:#}"))?;
        vault.delete(&id, &stamp()).map_err(|e| format!("{e:#}"))?;
        Ok(format!(
            "Deleted \"{}\" from the vault — a backup was kept",
            entry.name
        ))
    })
    .await
}

#[tauri::command]
async fn restore(path: String) -> Result<String, String> {
    blocking(move || {
        let vault = open_vault()?;
        let entry = vault
            .restore_snapshot(std::path::Path::new(&path), &stamp())
            .map_err(|e| format!("{e:#}"))?;
        Ok(format!("\"{}\" is back in the vault", entry.name))
    })
    .await
}

/// Import a `.mcworld` file (from Downloads, a Switch export, anywhere).
#[tauri::command]
async fn import(path: String) -> Result<String, String> {
    blocking(move || {
        let vault = open_vault()?;
        let source = PathBuf::from(&path);
        let entry = if source.is_dir() {
            vault.import_folder(&source, &stamp())
        } else {
            vault.import_mcworld(&source, &stamp())
        }
        .map_err(|e| format!("{e:#}"))?;
        Ok(format!("Added \"{}\" to the vault", entry.name))
    })
    .await
}

#[tauri::command]
async fn export(id: String) -> Result<String, String> {
    blocking(move || {
        let vault = open_vault()?;
        let entry = vault.entry(&id).map_err(|e| format!("{e:#}"))?;
        let safe: String = entry
            .name
            .chars()
            .map(|c| if r#"\/:*?"<>|"#.contains(c) { '_' } else { c })
            .collect();
        let out = vault.exports_dir().join(format!("{safe}.mcworld"));
        bedrock_vault_core::mcworld::pack(&entry.world_dir, &out).map_err(|e| format!("{e:#}"))?;
        Ok(format!("Saved a copy to {}", out.display()))
    })
    .await
}

#[derive(Serialize)]
struct RealmDto {
    id: i64,
    name: String,
    state: String,
    subscription: String,
    expired: bool,
    active_slot: Option<i64>,
    max_players: Option<i64>,
    /// "yours", "joined", or "?" when it cannot be determined.
    role: &'static str,
    /// Only a Realm's owner may download or replace its world.
    can_download: bool,
}

#[derive(Serialize)]
struct RealmsDto {
    signed_in: bool,
    gamertag: Option<String>,
    realms: Vec<RealmDto>,
    /// Present when the account is signed in but the listing failed.
    error: Option<String>,
}

#[derive(Serialize)]
struct LoginDto {
    user_code: String,
    verification_uri: String,
    interval_secs: u64,
}

/// A device-code sign-in waiting for the user to finish at microsoft.com/link.
#[derive(Default)]
struct PendingLogin(std::sync::Mutex<Option<realms::auth::DeviceLogin>>);

#[tauri::command]
async fn realms_overview() -> Result<RealmsDto, String> {
    blocking(|| {
        if !realms::cache::is_signed_in() {
            return Ok(RealmsDto {
                signed_in: false,
                gamertag: None,
                realms: Vec::new(),
                error: None,
            });
        }
        let session = match realms::session(false) {
            Ok(s) => s,
            // Signed in but the token could not be renewed: report it rather
            // than silently showing an empty list.
            Err(e) => {
                return Ok(RealmsDto {
                    signed_in: true,
                    gamertag: None,
                    realms: Vec::new(),
                    error: Some(format!("{e:#}")),
                })
            }
        };
        let gamertag = session.gamertag.clone();
        match realms::list(&session) {
            Ok(list) => Ok(RealmsDto {
                signed_in: true,
                gamertag,
                realms: list
                    .into_iter()
                    .map(|r| RealmDto {
                        subscription: r.subscription(),
                        role: r.role(),
                        can_download: r.owner == Some(true),
                        id: r.id,
                        name: r.name,
                        state: r.state,
                        expired: r.expired,
                        active_slot: r.active_slot,
                        max_players: r.max_players,
                    })
                    .collect(),
                error: None,
            }),
            Err(e) => Ok(RealmsDto {
                signed_in: true,
                gamertag,
                realms: Vec::new(),
                error: Some(format!("{e:#}")),
            }),
        }
    })
    .await
}

#[tauri::command]
async fn realms_begin_login(pending: tauri::State<'_, PendingLogin>) -> Result<LoginDto, String> {
    let login = blocking(|| realms::auth::start_device_login().map_err(|e| format!("{e:#}"))).await?;
    let dto = LoginDto {
        user_code: login.user_code.clone(),
        verification_uri: login.verification_uri.clone(),
        interval_secs: login.interval_secs,
    };
    *pending.0.lock().map_err(|_| "sign-in state was poisoned")? = Some(login);
    Ok(dto)
}

/// One poll of a sign-in in progress. `None` means the user has not finished.
#[tauri::command]
async fn realms_poll_login(
    pending: tauri::State<'_, PendingLogin>,
) -> Result<Option<String>, String> {
    let device_code = {
        let guard = pending.0.lock().map_err(|_| "sign-in state was poisoned")?;
        guard.as_ref().map(|l| l.device_code.clone())
    };
    let device_code = device_code.ok_or("no sign-in is in progress")?;

    let tokens = blocking(move || {
        realms::auth::poll_device_login(&device_code).map_err(|e| format!("{e:#}"))
    })
    .await?;

    let Some(tokens) = tokens else { return Ok(None) };
    let session =
        blocking(move || realms::complete_login(tokens).map_err(|e| format!("{e:#}"))).await?;
    *pending.0.lock().map_err(|_| "sign-in state was poisoned")? = None;
    Ok(Some(session.gamertag.unwrap_or_else(|| "your account".into())))
}

#[tauri::command]
async fn realms_cancel_login(pending: tauri::State<'_, PendingLogin>) -> Result<(), String> {
    *pending.0.lock().map_err(|_| "sign-in state was poisoned")? = None;
    Ok(())
}

/// Pull a Realm slot's world into the vault. Read-only on Mojang's side.
#[tauri::command]
async fn realm_download(
    app: tauri::AppHandle,
    realm_id: i64,
    slot: Option<i64>,
    name: String,
) -> Result<String, String> {
    blocking(move || {
        let vault = open_vault()?;
        let session = realms::session(false).map_err(|e| format!("{e:#}"))?;

        let slot = match slot {
            Some(s) => s,
            None => realms::list(&session)
                .map_err(|e| format!("{e:#}"))?
                .into_iter()
                .find(|r| r.id == realm_id)
                .and_then(|r| r.active_slot)
                .ok_or("could not tell which slot to download")?,
        };

        let download = realms::slot_download(&session, realm_id, slot, None)
            .map_err(|e| format!("{e:#}"))?;

        let temp = vault
            .exports_dir()
            .join(format!("realm-{realm_id}-slot{slot}.mcworld"));
        let mut last = 0u64;
        realms::fetch_world(&download, &temp, |done, total| {
            // Report per megabyte: an event per 64 KB chunk would flood the UI.
            if done - last > 1024 * 1024 || done >= total {
                last = done;
                let _ = app.emit(
                    "progress",
                    ProgressDto {
                        done: (done / 1024 / 1024) as usize,
                        total: (total / 1024 / 1024).max(1) as usize,
                        current: name.clone(),
                    },
                );
            }
        })
        .map_err(|e| format!("{e:#}"))?;

        let entry = vault
            .import_mcworld(&temp, &stamp())
            .map_err(|e| format!("{e:#}"))?;
        std::fs::remove_file(&temp).ok();
        Ok(format!("\"{}\" is now in your vault", entry.name))
    })
    .await
}

#[tauri::command]
async fn realms_sign_out() -> Result<String, String> {
    blocking(|| {
        realms::cache::clear().map_err(|e| format!("{e:#}"))?;
        Ok("Signed out".to_owned())
    })
    .await
}

/// Open a link in the default browser.
#[tauri::command]
async fn open_url(url: String) -> Result<String, String> {
    // Only ever our own sign-in page; never a value from the page itself.
    if url != "https://www.microsoft.com/link" && !url.starts_with("https://login.live.com/") {
        return Err("refused to open that address".into());
    }
    std::process::Command::new("explorer")
        .arg(&url)
        .spawn()
        .map_err(|e| format!("could not open the browser: {e}"))?;
    Ok(String::new())
}

/// Move the vault to a new folder, or adopt a vault that is already there.
///
/// An empty destination gets the current vault moved into it; a folder that
/// already holds a vault is simply adopted, so a vault on a shared drive can be
/// picked up on another PC.
#[tauri::command]
async fn set_vault_location(app: tauri::AppHandle, path: String) -> Result<String, String> {
    blocking(move || {
        let dest = PathBuf::from(&path);
        let current = vault_root();
        if dest == current {
            return Ok("That is already where the vault lives".to_owned());
        }

        if vault::looks_like_vault(&dest) {
            config::set_vault_root(&dest).map_err(|e| format!("{e:#}"))?;
            return Ok(format!("Now using the vault already in {}", dest.display()));
        }

        if !vault::is_empty_dir(&dest) {
            return Err(format!(
                "{} already has files in it. Pick an empty folder, or one that already holds a vault.",
                dest.display()
            ));
        }

        if vault::looks_like_vault(&current) {
            let moved = vault::move_vault(&current, &dest, |done, total| {
                let _ = app.emit(
                    "progress",
                    ProgressDto { done: done as usize, total: total as usize, current: String::new() },
                );
            })
            .map_err(|e| format!("{e:#}"))?;
            config::set_vault_root(&dest).map_err(|e| format!("{e:#}"))?;
            return Ok(format!("Moved {moved} files to {}", dest.display()));
        }

        config::set_vault_root(&dest).map_err(|e| format!("{e:#}"))?;
        Ok(format!("The vault is now at {}", dest.display()))
    })
    .await
}

/// Open a vault sub-folder in Explorer.
#[tauri::command]
async fn open_folder(which: String) -> Result<String, String> {
    let root = vault_root();
    let path = match which.as_str() {
        "exports" => root.join("exports"),
        "backups" => root.join("backups"),
        _ => root,
    };
    std::process::Command::new("explorer")
        .arg(&path)
        .spawn()
        .map_err(|e| format!("could not open Explorer: {e}"))?;
    Ok(format!("Opened {}", path.display()))
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(PendingLogin::default())
        .invoke_handler(tauri::generate_handler![
            overview, game_status, save_to_vault, save_all, put_away, play, back_up, delete,
            restore, import, export, open_folder, set_vault_location, realms_overview,
            realms_begin_login, realms_poll_login, realms_cancel_login, realms_sign_out,
            realm_download, open_url
        ])
        .run(tauri::generate_context!())
        .expect("error while running Bedrock Vault");
}
