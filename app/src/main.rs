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
    /// Where this world last came from or last went — "minecraft", "realm",
    /// "file", "backup", or absent for one untouched since the vault started
    /// recording it.
    source: Option<String>,
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
                index.insert(p.uuid.replace('-', ""), p.name.clone());
                index.insert(p.uuid, p.name);
            }
        }
    }
    index
}

/// Pack names for a world, plus a count of ones nothing here can name.
///
/// A world's own pack history names its add-ons even when they were never
/// downloaded to this PC, so that is consulted before the installed cache.
fn world_packs(world_dir: &std::path::Path, index: &HashMap<String, String>) -> (Vec<String>, usize) {
    let history: HashMap<String, String> =
        packs::names_from_history(world_dir).into_iter().collect();

    let (rp, bp) = packs::world_pack_refs(world_dir);
    let mut names = Vec::new();
    let mut unknown = 0;
    for r in rp.into_iter().chain(bp) {
        let key = r.uuid.replace('-', "");
        match history.get(&key).or_else(|| index.get(&r.uuid)).or_else(|| index.get(&key)) {
            Some(name) => names.push(name.clone()),
            None => unknown += 1,
        }
    }
    names.sort();
    names.dedup();
    (names, unknown)
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
                source: e.source.clone(),
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
        let taken = vault
            .snapshot(&id, &entry.world_dir, &stamp())
            .map_err(|e| format!("{e:#}"))?;
        Ok(match taken {
            Some(_) => format!("Backed up \"{}\"", entry.name),
            None => format!(
                "\"{}\" has not changed since the last backup, so there was no need to make another",
                entry.name
            ),
        })
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

/// Delete one backup for good. The world it came from is not touched.
#[tauri::command]
async fn delete_backup(path: String) -> Result<String, String> {
    blocking(move || {
        let vault = open_vault()?;
        vault
            .delete_snapshot(std::path::Path::new(&path))
            .map_err(|e| format!("{e:#}"))?;
        Ok("That backup is gone".to_owned())
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
struct SlotDto {
    slot_id: i64,
    name: String,
    game_mode: Option<String>,
    difficulty: Option<String>,
    hardcore: bool,
    seed: Option<String>,
    active: bool,
    /// No world has ever been put on this slot.
    empty: bool,
    /// Whether Minecraft will hand this slot's world over, which is what makes
    /// a backup-before-replacing possible.
    can_backup: bool,
    /// True when the copy Minecraft holds for this slot is a *different* world
    /// from the one on it now — a previous occupant, never re-backed up after
    /// the swap. `null` when nothing available says either way.
    stored_is_older_world: Option<bool>,
    /// Pack names where installed locally, otherwise a short id.
    packs: Vec<PackRefDto>,
    rules: Vec<[String; 2]>,
    /// The world's own picture, from a copy of it held on this PC, or the
    /// artwork of the marketplace template it was built from.
    icon: Option<String>,
}

#[derive(Serialize)]
struct PackRefDto {
    name: String,
    icon: Option<String>,
    installed: bool,
}

#[derive(Serialize)]
struct PlayerDto {
    name: String,
    role: Option<String>,
    online: bool,
    last_login: Option<i64>,
}

#[derive(Serialize)]
struct RealmsDto {
    signed_in: bool,
    profile: Option<ProfileDto>,
    /// Owned and still subscribed — the ones that can actually be managed.
    mine: Vec<RealmDto>,
    /// Owned but lapsed. Worlds are still downloadable.
    expired: Vec<RealmDto>,
    /// Someone else's Realm this account has joined.
    joined: Vec<RealmDto>,
    /// Present when the account is signed in but the listing failed.
    error: Option<String>,
}

#[derive(Serialize, Clone)]
struct ProfileDto {
    gamertag: String,
    picture: Option<String>,
    gamerscore: Option<String>,
}

#[derive(Serialize)]
struct PackDto {
    uuid: String,
    name: String,
    description: Option<String>,
    version: String,
    category: String,
    icon: Option<String>,
    size_bytes: u64,
    /// Worlds in Minecraft or the vault that use this pack.
    used_by: Vec<String>,
}

#[derive(Serialize)]
struct PacksDto {
    packs: Vec<PackDto>,
    persona_count: usize,
    total_bytes: u64,
    /// Packs referenced by a world but not present on this PC.
    missing: Vec<String>,
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

fn profile_dto() -> Option<ProfileDto> {
    realms::who_am_i().map(|p| ProfileDto {
        gamertag: p.gamertag,
        picture: p.picture,
        gamerscore: p.gamerscore,
    })
}

/// Who is signed in, for the window header. Cheap: served from the cache.
#[tauri::command]
async fn profile() -> Result<Option<ProfileDto>, String> {
    blocking(|| Ok(profile_dto())).await
}

#[tauri::command]
async fn realms_overview() -> Result<RealmsDto, String> {
    blocking(|| {
        let empty = |error: Option<String>, signed_in: bool| RealmsDto {
            signed_in,
            profile: if signed_in { profile_dto() } else { None },
            mine: Vec::new(),
            expired: Vec::new(),
            joined: Vec::new(),
            error,
        };
        if !realms::cache::is_signed_in() {
            return Ok(empty(None, false));
        }
        // Signed in but the token could not be renewed: say so rather than
        // silently showing an empty list.
        let session = match realms::session(false) {
            Ok(s) => s,
            Err(e) => return Ok(empty(Some(format!("{e:#}")), true)),
        };

        let list = match realms::list(&session) {
            Ok(list) => list,
            Err(e) => return Ok(empty(Some(format!("{e:#}")), true)),
        };

        let to_dto = |r: realms::Realm| RealmDto {
            subscription: r.subscription(),
            role: r.role(),
            can_download: r.owner == Some(true),
            id: r.id,
            name: r.name,
            state: r.state,
            expired: r.expired,
            active_slot: r.active_slot,
            max_players: r.max_players,
        };

        let mut dto = empty(None, true);
        for realm in list {
            let owned = realm.owner == Some(true);
            match (owned, realm.expired) {
                (true, false) => dto.mine.push(to_dto(realm)),
                (true, true) => dto.expired.push(to_dto(realm)),
                (false, _) => dto.joined.push(to_dto(realm)),
            }
        }
        Ok(dto)
    })
    .await
}

/// Everything about one Realm: slots, worlds, packs and players.
#[tauri::command]
async fn realm_detail(realm_id: i64) -> Result<serde_json::Value, String> {
    blocking(move || {
        let session = realms::session(false).map_err(|e| format!("{e:#}"))?;
        let detail = realms::detail(&session, realm_id).map_err(|e| format!("{e:#}"))?;
        let known = known_packs();
        let by_seed = worlds_by_seed();

        let slots: Vec<SlotDto> = detail
            .slots
            .into_iter()
            .map(|s| {
                // One request answers both questions: whether a copy exists at
                // all, and whether it is this world or a previous occupant.
                let stored = if s.empty {
                    None
                } else {
                    realms::stored_world(&session, realm_id, s.slot_id)
                };
                SlotDto {
                    name: s.name.clone().unwrap_or_else(|| "Unnamed world".into()),
                    packs: name_slot_packs(&s, &known, &by_seed),
                    icon: slot_picture(&s, &by_seed),
                    rules: s.rules.iter().map(|(k, v)| [k.clone(), v.clone()]).collect(),
                    can_backup: stored.is_some(),
                    stored_is_older_world: stored
                        .as_ref()
                        .and_then(|st| st.matches_slot(&s))
                        .map(|same| !same),
                    slot_id: s.slot_id,
                    game_mode: s.game_mode,
                    difficulty: s.difficulty,
                    hardcore: s.hardcore,
                    seed: s.seed,
                    active: s.active,
                    empty: s.empty,
                }
            })
            .collect();

        let players: Vec<PlayerDto> = detail
            .players
            .into_iter()
            .map(|p| PlayerDto {
                name: p.name.unwrap_or_else(|| "Player".into()),
                role: p.role,
                online: p.online,
                last_login: p.last_login,
            })
            .collect();

        serde_json::to_value(serde_json::json!({
            "id": detail.realm.id,
            "name": detail.realm.name,
            "state": detail.realm.state,
            "subscription": detail.realm.subscription(),
            "expired": detail.realm.expired,
            "max_players": detail.realm.max_players,
            "slots": slots,
            "players": players,
            "product": detail.product,
        }))
        .map_err(|e| e.to_string())
    })
    .await
}

/// World seed -> every copy of that world held here.
///
/// A Realm slot reports its seed, which identifies which world is on it far
/// more reliably than a name that anyone can change. All the copies are kept,
/// not just the first: the same world can sit in the vault and in the game at
/// once, and only one of them may carry the picture or the pack history.
fn worlds_by_seed() -> HashMap<String, Vec<PathBuf>> {
    let mut index: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let mut note = |dir: PathBuf| {
        if let Some(seed) = std::fs::read(dir.join("level.dat"))
            .ok()
            .and_then(|d| bedrock_vault_core::level_dat::parse(&d).ok())
            .and_then(|m| m.seed)
        {
            index.entry(seed.to_string()).or_default().push(dir);
        }
    };
    if let Ok(vault) = open_vault() {
        for e in vault.list().unwrap_or_default() {
            note(e.world_dir);
        }
    }
    if let Ok(locations) = scan::find_worlds_dirs() {
        for loc in locations.iter().filter(|l| l.path.is_dir()) {
            for w in scan::scan(&loc.path).unwrap_or_default() {
                note(loc.path.join(&w.folder));
            }
        }
    }
    index
}

/// Copies of a slot's world held on this PC, matched on the seed.
fn local_copies<'a>(
    slot: &realms::Slot,
    by_seed: &'a HashMap<String, Vec<PathBuf>>,
) -> &'a [PathBuf] {
    slot.seed
        .as_ref()
        .and_then(|seed| by_seed.get(seed))
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// A picture for a Realm slot, best first.
///
/// Nothing Mojang serves shows what a Realm world looks like: `thumbnailId` is
/// null on every Realm, and the archive a slot hands over has no
/// `world_icon.jpeg` in it. But a world played on this PC has one, and a slot's
/// seed says which local world is the same world — so the picture is the real
/// one Minecraft draws for it, not an invention. Failing that, a world built
/// from a marketplace template has the template's artwork, which is the only
/// image the service itself ever offers.
fn slot_picture(slot: &realms::Slot, by_seed: &HashMap<String, Vec<PathBuf>>) -> Option<String> {
    if let Some(icon) = local_copies(slot, by_seed).iter().find_map(|dir| world_icon(dir)) {
        return Some(icon);
    }
    let url = slot.template_image.as_deref()?;
    realms::profile::fetch_data_uri(url).ok()
}

/// Put names to a slot's add-ons.
///
/// Realms mints its own ids for packs attached to a slot; they appear nowhere
/// locally and no endpoint resolves them. But if a copy of that world is held
/// here — matched on the seed the slot reports — the world's own pack history
/// names them, and their order matches the slot's. Where that fails, the kind
/// of pack is still known, so it says "Resource pack" rather than pretending.
fn name_slot_packs(
    slot: &realms::Slot,
    known: &HashMap<String, (String, Option<String>)>,
    by_seed: &HashMap<String, Vec<PathBuf>>,
) -> Vec<PackRefDto> {
    let from_world: Vec<String> = local_copies(slot, by_seed)
        .iter()
        .map(|dir| {
            let mut names: Vec<String> = packs::names_from_history(dir)
                .into_iter()
                .map(|(_, name)| name)
                .collect();
            names.dedup();
            names
        })
        .find(|names: &Vec<String>| !names.is_empty())
        .unwrap_or_default();

    slot.pack_ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            if let Some(p) = known.get(&id.replace('-', "")) {
                return PackRefDto { name: p.0.clone(), icon: p.1.clone(), installed: true };
            }
            match from_world.get(i).or_else(|| from_world.first()) {
                Some(name) => PackRefDto { name: name.clone(), icon: None, installed: true },
                None => PackRefDto { name: "Add-on".to_owned(), icon: None, installed: false },
            }
        })
        .collect()
}

/// uuid (dashless) -> (name, icon) for every pack this PC can name.
///
/// Two sources, because neither is complete on its own: packs installed from
/// the store (which have artwork), and the names worlds remember in their pack
/// history (which cover marketplace content that was never downloaded here —
/// the usual case for a world living on a Realm).
fn known_packs() -> HashMap<String, (String, Option<String>)> {
    let mut index: HashMap<String, (String, Option<String>)> = HashMap::new();

    let mut from_history = |world_dir: &std::path::Path| {
        for (id, name) in packs::names_from_history(world_dir) {
            index.entry(id).or_insert((name, None));
        }
    };
    if let Ok(locations) = scan::find_worlds_dirs() {
        for loc in locations.iter().filter(|l| l.path.is_dir()) {
            for w in scan::scan(&loc.path).unwrap_or_default() {
                from_history(&loc.path.join(&w.folder));
            }
        }
    }
    if let Ok(vault) = open_vault() {
        for e in vault.list().unwrap_or_default() {
            from_history(&e.world_dir);
        }
    }

    // Installed packs win: they carry artwork as well as a name.
    for (_, cache) in packs::find_premium_caches() {
        if let Ok((found, _)) = packs::scan_premium_cache(&cache) {
            for p in found {
                index.insert(p.uuid.replace('-', ""), (p.name, p.icon));
            }
        }
    }
    index
}

/// Marketplace content installed on this PC, with what uses it.
#[tauri::command]
async fn packs_overview() -> Result<PacksDto, String> {
    blocking(|| {
        // Which worlds use which pack, so a pack can say where it is in use.
        let mut used_by: HashMap<String, Vec<String>> = HashMap::new();
        let mut referenced: Vec<String> = Vec::new();

        let mut note = |dir: &std::path::Path, world: &str| {
            let (rp, bp) = packs::world_pack_refs(dir);
            for r in rp.into_iter().chain(bp) {
                let key = r.uuid.replace('-', "");
                referenced.push(key.clone());
                let entry = used_by.entry(key).or_default();
                if !entry.iter().any(|w| w == world) {
                    entry.push(world.to_owned());
                }
            }
        };

        if let Ok(locations) = scan::find_worlds_dirs() {
            for loc in locations.iter().filter(|l| l.path.is_dir()) {
                if let Ok(worlds) = scan::scan(&loc.path) {
                    for w in worlds {
                        note(&loc.path.join(&w.folder), &w.display_name());
                    }
                }
            }
        }
        if let Ok(vault) = open_vault() {
            for e in vault.list().unwrap_or_default() {
                note(&e.world_dir, &e.name);
            }
        }

        let mut all = Vec::new();
        let mut persona_count = 0;
        let mut installed_ids = std::collections::HashSet::new();
        for (_, cache) in packs::find_premium_caches() {
            if let Ok((found, persona)) = packs::scan_premium_cache(&cache) {
                persona_count += persona;
                for p in found {
                    let key = p.uuid.replace('-', "");
                    installed_ids.insert(key.clone());
                    all.push(PackDto {
                        used_by: used_by.get(&key).cloned().unwrap_or_default(),
                        category: packs::CATEGORIES
                            .iter()
                            .find(|(dir, _)| *dir == p.category)
                            .map(|(_, title)| (*title).to_owned())
                            .unwrap_or_else(|| "Add-on".to_owned()),
                        uuid: p.uuid,
                        name: p.name,
                        description: p.description,
                        version: p.version,
                        icon: p.icon,
                        size_bytes: p.size_bytes,
                    });
                }
            }
        }
        // Templates bundle copies of their own packs; show the template only.
        all.sort_by(|a, b| a.category.cmp(&b.category).then(a.name.cmp(&b.name)));

        let mut missing: Vec<String> = referenced
            .into_iter()
            .filter(|id| !installed_ids.contains(id))
            .collect();
        missing.sort();
        missing.dedup();

        Ok(PacksDto {
            total_bytes: all.iter().map(|p| p.size_bytes).sum(),
            packs: all,
            persona_count,
            missing,
        })
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

        // What the slot says it is holding, to check the archive against once
        // it lands. Only the seed is worth trusting: a name can be anything.
        let expected = realms::detail(&session, realm_id)
            .ok()
            .and_then(|d| d.slots.into_iter().find(|s| s.slot_id == slot))
            .map(|s| (s.name, s.seed));

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

        // Minecraft serves its own last backup of the slot, which can predate
        // the world now on it, so say plainly when a different world arrived
        // rather than banking it under the name that was clicked.
        let arrived_seed = bedrock_vault_core::mcworld::seed_in_archive(&temp);
        let wrong_world = match (&expected, arrived_seed) {
            (Some((_, Some(want))), Some(got)) => want.parse::<i64>().ok() != Some(got),
            _ => false,
        };

        let entry = vault
            .import_mcworld(&temp, &stamp())
            .map_err(|e| format!("{e:#}"))?;
        std::fs::remove_file(&temp).ok();
        vault
            .set_source(&entry.id, vault::SOURCE_REALM)
            .map_err(|e| format!("{e:#}"))?;

        if wrong_world {
            let on_the_slot = expected
                .and_then(|(name, _)| name)
                .unwrap_or_else(|| name.clone());
            return Ok(format!(
                "Minecraft's saved copy of slot {slot} is an older world — \"{}\", not \"{}\" — \
                 so that is what came into your vault. Minecraft only saves a slot again once \
                 somebody plays there.",
                entry.name, on_the_slot
            ));
        }
        Ok(format!("\"{}\" is now in your vault", entry.name))
    })
    .await
}

/// Rename the world in a Realm slot.
#[tauri::command]
async fn realm_rename_slot(realm_id: i64, slot: i64, name: String) -> Result<String, String> {
    blocking(move || {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Err("give the world a name".into());
        }
        let session = realms::session(false).map_err(|e| format!("{e:#}"))?;
        realms::set_slot_name(&session, realm_id, slot, &name).map_err(|e| format!("{e:#}"))?;
        Ok(format!("The world on slot {slot} is now called \"{name}\""))
    })
    .await
}

/// Switch which slot the Realm runs.
#[tauri::command]
async fn realm_switch_slot(realm_id: i64, slot: i64) -> Result<String, String> {
    blocking(move || {
        let session = realms::session(false).map_err(|e| format!("{e:#}"))?;
        realms::switch_to_slot(&session, realm_id, slot).map_err(|e| format!("{e:#}"))?;
        Ok(format!("The Realm is now playing slot {slot}"))
    })
    .await
}

/// Turn off every add-on on a Realm slot.
#[tauri::command]
async fn realm_clear_packs(realm_id: i64, slot: i64) -> Result<String, String> {
    blocking(move || {
        let session = realms::session(false).map_err(|e| format!("{e:#}"))?;
        realms::clear_slot_packs(&session, realm_id, slot).map_err(|e| format!("{e:#}"))?;
        Ok(format!("Add-ons turned off on slot {slot}"))
    })
    .await
}

/// Realms this account may push a world onto: owned and still subscribed.
#[tauri::command]
async fn realm_targets() -> Result<Vec<RealmDto>, String> {
    blocking(|| {
        if !realms::cache::is_signed_in() {
            return Ok(Vec::new());
        }
        let session = realms::session(false).map_err(|e| format!("{e:#}"))?;
        Ok(realms::list(&session)
            .map_err(|e| format!("{e:#}"))?
            .into_iter()
            .filter(|r| r.owner == Some(true) && !r.expired)
            .map(|r| RealmDto {
                subscription: r.subscription(),
                role: r.role(),
                can_download: true,
                id: r.id,
                name: r.name,
                state: r.state,
                expired: r.expired,
                active_slot: r.active_slot,
                max_players: r.max_players,
            })
            .collect())
    })
    .await
}

/// Put a vault world onto a Realm slot, replacing what is there.
///
/// The Realm's current world is downloaded into the vault first; see
/// `realms::replace_slot_world` for why that order is fixed.
#[tauri::command]
async fn realm_upload(
    app: tauri::AppHandle,
    realm_id: i64,
    slot: Option<i64>,
    id: String,
) -> Result<String, String> {
    blocking(move || {
        let vault = open_vault()?;
        let entry = vault.entry(&id).map_err(|e| format!("{e:#}"))?;
        let session = realms::session(false).map_err(|e| format!("{e:#}"))?;

        let realm = realms::list(&session)
            .map_err(|e| format!("{e:#}"))?
            .into_iter()
            .find(|r| r.id == realm_id)
            .ok_or("that Realm is no longer on this account")?;
        if realm.owner != Some(true) {
            return Err("only a Realm's owner can replace its world".into());
        }
        let slot = slot
            .or(realm.active_slot)
            .ok_or("could not tell which slot to replace")?;
        // Back up first whenever there is something Minecraft will actually
        // hand over — an empty slot has nothing, and a freshly-uploaded world
        // has no stored copy yet.
        let occupied = realms::detail(&session, realm_id)
            .map(|d| d.slots.iter().any(|s| s.slot_id == slot && !s.empty))
            .unwrap_or(true);
        let backup_first = occupied && realms::slot_is_downloadable(&session, realm_id, slot);

        let step = |app: &tauri::AppHandle, text: &str| {
            let _ = app.emit(
                "progress",
                ProgressDto { done: 0, total: 0, current: text.to_owned() },
            );
        };
        let app_for_steps = app.clone();
        let saved = realms::replace_slot_world(
            &session,
            &vault,
            &realms::Replacement {
                realm_id,
                slot,
                world_dir: &entry.world_dir,
                stamp: &stamp(),
                close_first: true,
                backup_first,
            },
            |text| step(&app_for_steps, text),
            |_, _| {},
        )
        .map_err(|e| format!("{e:#}"))?;

        // The vault copy is on a Realm now, which is where it was last.
        vault
            .set_source(&entry.id, vault::SOURCE_REALM)
            .map_err(|e| format!("{e:#}"))?;

        Ok(match saved {
            Some(saved) => format!(
                "\"{}\" is now on {} — its previous world was saved to your vault as \"{}\"",
                entry.name, realm.name, saved.name
            ),
            None if occupied => format!(
                "\"{}\" is now on {} slot {slot}. Minecraft had no saved copy of what was there, so none could be kept.",
                entry.name, realm.name
            ),
            None => format!("\"{}\" is now on {} slot {slot}", entry.name, realm.name),
        })
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
            restore, delete_backup, import, export, open_folder, set_vault_location, realms_overview,
            realms_begin_login, realms_poll_login, realms_cancel_login, realms_sign_out,
            realm_download, realm_detail, realm_targets, realm_upload, realm_rename_slot,
            realm_clear_packs, realm_switch_slot, packs_overview, profile, open_url
        ])
        .run(tauri::generate_context!())
        .expect("error while running Bedrock Vault");
}
