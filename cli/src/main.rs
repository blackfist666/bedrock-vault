use std::path::PathBuf;

use anyhow::{Context, Result};
use bedrock_vault_core::{
    config, guard, level_dat, mcworld, packs, realms, scan,
    vault::{Protection, Vault},
};
use chrono::{Local, TimeZone};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "vault", version, about = "Bedrock Vault")]
struct Cli {
    /// Vault root directory (default: %USERPROFILE%\BedrockVault)
    #[arg(long, global = true)]
    vault: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scan minecraftWorlds and print a metadata table
    Scan {
        /// Directory to scan instead of the live minecraftWorlds
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// List installed store/marketplace content and per-world pack usage
    Packs,
    /// Report whether Minecraft is running (world operations are blocked if so)
    Guard,
    /// Sign in to a Microsoft account for Realms
    Login,
    /// Forget the signed-in Microsoft account
    Logout,
    /// Show the signed-in account
    Account {
        /// Re-authorise with Xbox even if the saved token is still valid
        #[arg(long)]
        refresh: bool,
        /// Print the claims Xbox returned about the account
        #[arg(long)]
        raw: bool,
    },
    /// List the Realms this account can see
    Realms {
        /// Print the service's raw JSON — the API is unofficial, so this is how
        /// to see what it actually returned when a field looks wrong
        #[arg(long)]
        raw: bool,
    },
    /// Show where the vault lives
    Where,
    /// Move the vault to another folder (or adopt a vault already there)
    Move {
        /// Destination folder
        dest: PathBuf,
    },
    /// List worlds held in the vault library
    Library,
    /// Copy a live world into the vault (or refresh its copy), keeping it in game
    Protect {
        /// Live world folder, or the folder id shown by `scan`; omit for all
        world: Option<String>,
    },
    /// List every backup, grouped by world
    Backups,
    /// List snapshot history for a vault world
    Snapshots {
        /// Library id shown by `library`
        id: String,
    },
    /// Rebuild a world from a snapshot as a new vault entry
    Restore {
        /// Path to a .mcworld snapshot
        snapshot: PathBuf,
    },
    /// Remove a world from the vault, keeping a final snapshot
    Delete {
        /// Library id shown by `library`
        id: String,
    },
    /// Protect a world and remove it from the in-game list
    Archive {
        /// Live world folder, or the folder id shown by `scan`
        world: String,
    },
    /// Copy a library world back into minecraftWorlds
    Activate {
        /// Library id shown by `library`
        id: String,
    },
    /// Import a .mcworld (or a world folder) into the library
    Import { path: PathBuf },
    /// Export a library world as a .mcworld into the vault's exports folder
    Export { id: String },
    /// Pack a world folder into a .mcworld
    Pack { world_dir: PathBuf, out: PathBuf },
    /// Unpack a .mcworld into a new folder and validate it
    Unpack { mcworld: PathBuf, dest: PathBuf },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let vault_root = cli.vault.clone();
    match cli.cmd {
        Cmd::Scan { dir } => cmd_scan(dir),
        Cmd::Packs => cmd_packs(),
        Cmd::Guard => {
            let status = guard::game_status();
            if status.is_running() {
                println!("Minecraft IS running ({}) — world operations blocked", status.running.join(", "));
            } else {
                println!("Minecraft is not running — safe to move world data");
            }
            Ok(())
        }
        Cmd::Login => cmd_login(),
        Cmd::Logout => {
            realms::cache::clear()?;
            println!("Signed out — the saved tokens have been deleted.");
            Ok(())
        }
        Cmd::Account { refresh, raw } => cmd_account(refresh, raw),
        Cmd::Realms { raw } => cmd_realms(raw),
        Cmd::Where => {
            let root = vault_root.clone().unwrap_or_else(config::vault_root);
            println!("Vault:  {}", root.display());
            println!("Config: {}", config::config_path()?.display());
            println!(
                "Status: {}",
                if bedrock_vault_core::vault::looks_like_vault(&root) {
                    "a vault is there"
                } else {
                    "no vault there yet (it will be created on first use)"
                }
            );
            Ok(())
        }
        Cmd::Move { dest } => cmd_move(vault_root, &dest),
        Cmd::Library => cmd_library(vault_root),
        Cmd::Protect { world } => cmd_protect(vault_root, world.as_deref()),
        Cmd::Backups => cmd_backups(vault_root),
        Cmd::Snapshots { id } => cmd_snapshots(vault_root, &id),
        Cmd::Restore { snapshot } => cmd_restore(vault_root, &snapshot),
        Cmd::Delete { id } => cmd_delete(vault_root, &id),
        Cmd::Archive { world } => cmd_archive(vault_root, &world),
        Cmd::Activate { id } => cmd_activate(vault_root, &id),
        Cmd::Import { path } => cmd_import(vault_root, &path),
        Cmd::Export { id } => cmd_export(vault_root, &id),
        Cmd::Pack { world_dir, out } => {
            let files = mcworld::pack(&world_dir, &out)?;
            println!(
                "Packed {} file(s) from {} into {} ({})",
                files,
                world_dir.display(),
                out.display(),
                human_size(std::fs::metadata(&out)?.len())
            );
            Ok(())
        }
        Cmd::Unpack { mcworld, dest } => {
            let files = mcworld::unpack(&mcworld, &dest)?;
            let meta = level_dat::parse(&std::fs::read(dest.join("level.dat"))?)?;
            println!(
                "Unpacked {} file(s) into {} — \"{}\" v{} ({})",
                files,
                dest.display(),
                meta.name.as_deref().unwrap_or("<unnamed>"),
                meta.version.as_deref().unwrap_or("?"),
                meta.game_mode_label(),
            );
            Ok(())
        }
    }
}

fn cmd_scan(dir: Option<PathBuf>) -> Result<()> {
    let locations = match dir {
        Some(d) => vec![scan::WorldsLocation { label: "custom".into(), path: d }],
        None => scan::find_worlds_dirs()?,
    };

    let mut grand_count = 0usize;
    let mut grand_size = 0u64;
    for loc in &locations {
        println!("[{}] {}", loc.label, loc.path.display());
        if !loc.path.is_dir() {
            println!("  (does not exist yet — the game creates it with the first local world)\n");
            continue;
        }
        let worlds = scan::scan(&loc.path)?;
        if worlds.is_empty() {
            println!("  (empty)\n");
            continue;
        }
        println!(
            "{:<14} {:<28} {:<9} {:<12} {:<16} {:>9}  {:<20}",
            "FOLDER", "NAME", "MODE", "VERSION", "LAST PLAYED", "SIZE", "SEED"
        );
        for w in &worlds {
            match &w.meta {
                Ok(m) => println!(
                    "{:<14} {:<28} {:<9} {:<12} {:<16} {:>9}  {:<20}",
                    truncate(&w.folder, 14),
                    truncate(&w.display_name(), 28),
                    m.game_mode_label(),
                    m.version.as_deref().unwrap_or("-"),
                    m.last_played.map(fmt_time).unwrap_or_else(|| "-".into()),
                    human_size(w.size_bytes),
                    m.seed.map(|s| s.to_string()).unwrap_or_else(|| "-".into()),
                ),
                Err(e) => println!(
                    "{:<14} {:<28} !! level.dat unreadable: {e:#}",
                    truncate(&w.folder, 14),
                    truncate(&w.display_name(), 28),
                ),
            }
        }
        let size: u64 = worlds.iter().map(|w| w.size_bytes).sum();
        println!("  {} world(s), {}\n", worlds.len(), human_size(size));
        grand_count += worlds.len();
        grand_size += size;
    }
    println!("Total: {} world(s), {}", grand_count, human_size(grand_size));
    Ok(())
}

fn open_vault(root: Option<PathBuf>) -> Result<Vault> {
    // --vault wins, otherwise whatever the app's config says.
    Vault::open(root.unwrap_or_else(config::vault_root))
}

/// Vault ids and folder names are timestamp-based, so operations are ordered
/// and readable on disk.
fn stamp() -> String {
    Local::now().format("%Y%m%d-%H%M%S").to_string()
}

fn cmd_login() -> Result<()> {
    let login = realms::auth::start_device_login()?;
    println!("\n  Go to:  {}", login.verification_uri);
    println!("  Enter:  {}\n", login.user_code);
    println!("Waiting for you to finish signing in (Ctrl+C to give up)…");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(login.expires_in_secs);
    let tokens = loop {
        std::thread::sleep(realms::auth::poll_delay(&login));
        if let Some(tokens) = realms::auth::poll_device_login(&login.device_code)? {
            break tokens;
        }
        if std::time::Instant::now() > deadline {
            anyhow::bail!("the code expired before sign-in finished");
        }
    };

    let session = realms::auth::realms_session(&tokens.access_token)?;
    let who = session.gamertag.clone().unwrap_or_else(|| "your account".into());
    realms::cache::save(&realms::cache::Session {
        microsoft: Some(tokens),
        realms: Some(session),
    })?;
    println!("\nSigned in as {who}.");
    Ok(())
}

/// The signed-in session, refreshed if the Realms token has aged out.
fn realms_session() -> Result<realms::auth::XstsToken> {
    session_with_refresh(false)
}

fn session_with_refresh(force: bool) -> Result<realms::auth::XstsToken> {
    let mut session = realms::cache::load();
    let microsoft = session
        .microsoft
        .clone()
        .context("not signed in — run `vault login` first")?;

    if let Some(realms_token) = &session.realms {
        // A token saved before the XUID was captured cannot answer "is this
        // Realm mine?", so treat it as stale too.
        if !force && !realms_token.is_expired() && realms_token.xuid.is_some() {
            return Ok(realms_token.clone());
        }
    }

    println!("Refreshing sign-in…");
    let refreshed = realms::auth::refresh(&microsoft.refresh_token)?;
    let token = realms::auth::realms_session(&refreshed.access_token)?;
    session.microsoft = Some(refreshed);
    session.realms = Some(token.clone());
    realms::cache::save(&session)?;
    Ok(token)
}

fn cmd_account(refresh: bool, raw: bool) -> Result<()> {
    let session = session_with_refresh(refresh)?;
    if raw {
        println!(
            "{}",
            serde_json::to_string_pretty(&session.claims.unwrap_or(serde_json::Value::Null))?
        );
        return Ok(());
    }
    println!(
        "Gamertag: {}",
        session.gamertag.as_deref().unwrap_or("(not supplied by Xbox)")
    );
    // Masked: an XUID identifies a real person and should not end up in logs
    // or screenshots.
    println!(
        "Account:  {}",
        match &session.xuid {
            Some(x) if x.len() > 4 => format!("…{} (XUID)", &x[x.len() - 4..]),
            Some(_) => "(short XUID)".to_owned(),
            None => "(no XUID — Realm ownership cannot be determined)".to_owned(),
        }
    );
    println!("Game:     {}", realms::client_version());
    Ok(())
}

fn cmd_realms(raw: bool) -> Result<()> {
    let session = realms_session()?;
    if raw {
        println!("{}", serde_json::to_string_pretty(&realms::list_raw(&session)?)?);
        return Ok(());
    }
    match &session.gamertag {
        Some(tag) => println!("Signed in as {tag} (client version {})\n", realms::client_version()),
        None => println!("Signed in (client version {})\n", realms::client_version()),
    }

    let realms = realms::list(&session)?;
    if realms.is_empty() {
        println!("No Realms on this account.");
        println!("(That still means sign-in worked — the service answered with an empty list.)");
        return Ok(());
    }

    println!(
        "{:<10} {:<28} {:<8} {:<6} {:<14} {:<8} YOURS",
        "ID", "NAME", "STATE", "SLOT", "SUBSCRIPTION", "PLAYERS"
    );
    for r in &realms {
        println!(
            "{:<10} {:<28} {:<8} {:<6} {:<14} {:<8} {}",
            r.id,
            truncate(&r.name, 28),
            r.state,
            r.active_slot.map(|s| s.to_string()).unwrap_or_else(|| "-".into()),
            r.subscription(),
            r.max_players.map(|m| m.to_string()).unwrap_or_else(|| "-".into()),
            r.role(),
        );
    }
    let active = realms.iter().filter(|r| !r.expired).count();
    let mine = realms.iter().filter(|r| r.owner == Some(true)).count();
    println!(
        "\n{} Realm(s) — {active} still subscribed, {} expired; {mine} yours, {} joined.",
        realms.len(),
        realms.len() - active,
        realms.iter().filter(|r| r.owner == Some(false)).count(),
    );
    if realms.iter().any(|r| r.owner == Some(false)) {
        println!("Only a Realm's owner can replace its world, so uploads will be limited to yours.");
    }
    Ok(())
}

fn cmd_move(root: Option<PathBuf>, dest: &std::path::Path) -> Result<()> {
    // An explicit --vault is a one-off override, so moving that vault must not
    // repoint the app at it; only a move of the configured vault updates config.
    let overridden = root.is_some();
    let current = root.unwrap_or_else(config::vault_root);

    if bedrock_vault_core::vault::looks_like_vault(dest) {
        if overridden {
            println!("A vault already exists at {} — nothing to do.", dest.display());
        } else {
            config::set_vault_root(dest)?;
            println!("Now using the vault already at {}", dest.display());
        }
        return Ok(());
    }

    println!("Moving {} → {}", current.display(), dest.display());
    let moved = bedrock_vault_core::vault::move_vault(&current, dest, |done, total| {
        if done % 200 == 0 || done == total {
            println!("  {done}/{total} files");
        }
    })?;
    if overridden {
        println!("Moved {moved} file(s) to {} (--vault given, so the app's own vault location is unchanged).", dest.display());
    } else {
        config::set_vault_root(dest)?;
        println!("Moved {moved} file(s). The vault now lives at {}", dest.display());
    }
    Ok(())
}

fn cmd_library(root: Option<PathBuf>) -> Result<()> {
    let vault = open_vault(root)?;
    println!("Vault: {}\n", vault.root.display());
    let entries = vault.list()?;
    if entries.is_empty() {
        println!("Library is empty — use `vault archive <world>` or `vault import <file>`.");
        return Ok(());
    }
    println!(
        "{:<18} {:<28} {:<9} {:<12} {:<16} {:>9}",
        "ID", "NAME", "MODE", "VERSION", "LAST PLAYED", "SIZE"
    );
    for e in &entries {
        println!(
            "{:<18} {:<28} {:<9} {:<12} {:<16} {:>9}",
            e.id,
            truncate(&e.name, 28),
            e.game_mode,
            e.version.as_deref().unwrap_or("-"),
            e.last_played.map(fmt_time).unwrap_or_else(|| "-".into()),
            human_size(e.size_bytes),
        );
    }
    let total: u64 = entries.iter().map(|e| e.size_bytes).sum();
    println!("\n{} world(s), {}", entries.len(), human_size(total));
    Ok(())
}

/// Accept either a full path or a `minecraftWorlds` folder id; returns both the
/// path and the folder id, which is how the vault links a copy to a live world.
fn resolve_live_world_with_id(world: &str) -> Result<(PathBuf, String)> {
    let direct = PathBuf::from(world);
    if direct.join("level.dat").is_file() {
        let id = direct
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .context("world path has no folder name")?;
        return Ok((direct, id));
    }
    for loc in scan::find_worlds_dirs()? {
        let candidate = loc.path.join(world);
        if candidate.join("level.dat").is_file() {
            return Ok((candidate, world.to_owned()));
        }
    }
    anyhow::bail!("no world folder found for '{world}'")
}

/// Protect one world, or every world that needs it.
fn cmd_protect(root: Option<PathBuf>, world: Option<&str>) -> Result<()> {
    let vault = open_vault(root)?;
    if let Some(world) = world {
        let (live, folder) = resolve_live_world_with_id(world)?;
        let entry = vault.protect(&live, &folder, &stamp())?;
        println!(
            "Protected \"{}\" as {} ({}) — still in your game.",
            entry.name,
            entry.id,
            human_size(entry.size_bytes)
        );
        return Ok(());
    }

    let mut done = 0;
    for loc in scan::find_worlds_dirs()? {
        if !loc.path.is_dir() {
            continue;
        }
        for w in scan::scan(&loc.path)? {
            let last_played = w.meta.as_ref().ok().and_then(|m| m.last_played);
            if vault.protection(&w.folder, last_played)? == Protection::Protected {
                continue;
            }
            let entry = vault.protect(&loc.path.join(&w.folder), &w.folder, &stamp())?;
            println!("  protected \"{}\" ({})", entry.name, human_size(entry.size_bytes));
            done += 1;
        }
    }
    println!(
        "{}",
        match done {
            0 => "Every world was already protected.".to_owned(),
            n => format!("Protected {n} world(s)."),
        }
    );
    Ok(())
}

fn cmd_backups(root: Option<PathBuf>) -> Result<()> {
    let vault = open_vault(root)?;
    let library = vault.list()?;
    let groups = vault.all_backups(&library);
    if groups.is_empty() {
        println!("No backups yet.");
        return Ok(());
    }
    let mut total = 0u64;
    for g in &groups {
        let size: u64 = g.snapshots.iter().map(|s| s.size_bytes).sum();
        total += size;
        println!(
            "{:<34} {:>3} backup(s)  {:>9}   newest {}",
            truncate(&g.name, 34),
            g.snapshots.len(),
            human_size(size),
            g.snapshots.first().map(|s| s.stamp.as_str()).unwrap_or("-"),
        );
    }
    println!("\n{} world(s) backed up, {}", groups.len(), human_size(total));
    Ok(())
}

fn cmd_snapshots(root: Option<PathBuf>, id: &str) -> Result<()> {
    let vault = open_vault(root)?;
    let entry = vault.entry(id)?;
    let snaps = vault.snapshots(&entry);
    println!("Snapshots for \"{}\" ({}):\n", entry.name, entry.id);
    if snaps.is_empty() {
        println!("  (none)");
        return Ok(());
    }
    for s in &snaps {
        println!("  {:<26} {:>9}  {}", s.stamp, human_size(s.size_bytes), s.path.display());
    }
    println!(
        "\n{} snapshot(s); retention keeps {} per world.",
        snaps.len(),
        vault.settings.snapshot_retention
    );
    Ok(())
}

fn cmd_restore(root: Option<PathBuf>, snapshot: &std::path::Path) -> Result<()> {
    let vault = open_vault(root)?;
    let entry = vault.restore_snapshot(snapshot, &stamp())?;
    println!(
        "Restored \"{}\" as {} — a new vault world; activate it to play it.",
        entry.name, entry.id
    );
    Ok(())
}

fn cmd_delete(root: Option<PathBuf>, id: &str) -> Result<()> {
    let vault = open_vault(root)?;
    let entry = vault.entry(id)?;
    let backup = vault.delete(id, &stamp())?;
    println!(
        "Removed \"{}\" from the vault. Final snapshot: {}",
        entry.name,
        backup.display()
    );
    Ok(())
}

fn cmd_archive(root: Option<PathBuf>, world: &str) -> Result<()> {
    let vault = open_vault(root)?;
    let (live, folder) = resolve_live_world_with_id(world)?;
    println!("Archiving {} …", live.display());
    let entry = vault.archive(&live, &folder, &stamp())?;
    println!(
        "Archived \"{}\" as {} ({}) — in the vault, out of the in-game list.",
        entry.name,
        entry.id,
        human_size(entry.size_bytes)
    );
    Ok(())
}

fn cmd_activate(root: Option<PathBuf>, id: &str) -> Result<()> {
    let vault = open_vault(root)?;
    let entry = vault.entry(id)?;
    let locations = scan::find_worlds_dirs()?;
    let target = locations
        .first()
        .context("no Minecraft install found to activate into")?;
    let dest = vault.activate(id, &target.path, &stamp())?;
    println!(
        "Activated \"{}\" into [{}] {} — it will appear in the in-game world list.",
        entry.name,
        target.label,
        dest.display()
    );
    if locations.len() > 1 {
        println!(
            "(note: {} world locations exist; used the first)",
            locations.len()
        );
    }
    Ok(())
}

fn cmd_import(root: Option<PathBuf>, path: &std::path::Path) -> Result<()> {
    let vault = open_vault(root)?;
    let entry = if path.is_dir() {
        vault.import_folder(path, &stamp())?
    } else {
        vault.import_mcworld(path, &stamp())?
    };
    println!(
        "Imported \"{}\" as {} ({})",
        entry.name,
        entry.id,
        human_size(entry.size_bytes)
    );
    Ok(())
}

fn cmd_export(root: Option<PathBuf>, id: &str) -> Result<()> {
    let vault = open_vault(root)?;
    let entry = vault.entry(id)?;
    let safe: String = entry
        .name
        .chars()
        .map(|c| if r#"\/:*?"<>|"#.contains(c) { '_' } else { c })
        .collect();
    let out = vault.exports_dir().join(format!("{safe}.mcworld"));
    let files = mcworld::pack(&entry.world_dir, &out)?;
    println!(
        "Exported {} file(s) to {} ({})",
        files,
        out.display(),
        human_size(std::fs::metadata(&out)?.len())
    );
    Ok(())
}

fn cmd_packs() -> Result<()> {
    let caches = packs::find_premium_caches();
    if caches.is_empty() {
        println!("No premium_cache found — no store content has been downloaded on this machine.");
    }

    // uuid -> name, across all caches, for the per-world join below.
    let mut index: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for (label, cache) in &caches {
        println!("[{label}] {}\n", cache.display());
        let (all, persona_count) = packs::scan_premium_cache(cache)?;
        for (dir_name, title) in packs::CATEGORIES {
            let of_kind: Vec<_> = all.iter().filter(|p| p.category == *dir_name).collect();
            if of_kind.is_empty() {
                continue;
            }
            println!("{} ({})", title.to_uppercase(), of_kind.len());
            for p in &of_kind {
                println!("  {:<44} {:<10} {}", truncate(&p.name, 44), p.version, p.uuid);
            }
            println!();
        }
        if persona_count > 0 {
            println!("Persona items: {persona_count} (character creator cosmetics)\n");
        }
        for p in all {
            index.insert(p.uuid.clone(), p.name);
        }
    }

    println!("WORLD PACK USAGE");
    let mut any = false;
    for loc in scan::find_worlds_dirs()? {
        if !loc.path.is_dir() {
            continue;
        }
        for world in scan::scan(&loc.path)? {
            let world_dir = loc.path.join(&world.folder);
            let (rp, bp) = packs::world_pack_refs(&world_dir);
            if rp.is_empty() && bp.is_empty() {
                continue;
            }
            any = true;
            println!("  {} [{}]", world.display_name(), loc.label);
            for (kind, refs) in [("resource", rp), ("behavior", bp)] {
                for r in refs {
                    match index.get(&r.uuid) {
                        Some(name) => println!("    {kind}: {name} ({})", r.version),
                        None => println!("    {kind}: {} ({}) — not in premium_cache", r.uuid, r.version),
                    }
                }
            }
        }
    }
    if !any {
        println!("  (no world references any pack)");
    }
    Ok(())
}

fn fmt_time(unix: i64) -> String {
    match Local.timestamp_opt(unix, 0) {
        chrono::LocalResult::Single(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        _ => format!("@{unix}"),
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max - 1).collect();
        format!("{cut}…")
    }
}
