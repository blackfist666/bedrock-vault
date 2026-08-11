//! The vault: a mirror of every world plus its snapshot history.
//!
//! Two independent ideas, deliberately kept apart:
//!
//! - **Protected** — a copy of the world lives in `library\`. This is the
//!   secure store and should be true for every world.
//! - **Active** — the world is also in `minecraftWorlds`, so it shows up in the
//!   in-game list. Purely the user's choice.
//!
//! Archiving is therefore just "protect, then take it out of the game", and
//! activating is "put a protected world back". Snapshots in `backups\` are the
//! undo history; the library copy is the current state.
//!
//! Layout (§5.1 of DESIGN.md):
//! ```text
//! <root>\library\<id>\world\   raw world folder, ready to copy
//!                    \meta.json
//!        \backups\<id>\<timestamp>.mcworld
//!        \exports\
//!        \settings.json
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::guard;
use crate::level_dat;
use crate::mcworld;
use crate::scan;

/// How a live world relates to its vault copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Protection {
    /// No copy in the vault at all.
    None,
    /// Vault copy matches the world as last played.
    Protected,
    /// Vault copy exists but the world has been played since.
    Stale,
}

impl Protection {
    pub fn label(self) -> &'static str {
        match self {
            Protection::None => "Not protected",
            Protection::Protected => "Protected",
            Protection::Stale => "Needs sync",
        }
    }

    /// Compare a live world against the vault copy that mirrors it.
    ///
    /// Takes the already-loaded entry so callers listing many worlds do not
    /// re-read the whole library once per world.
    pub fn compare(entry: Option<&LibraryEntry>, live_last_played: Option<i64>) -> Self {
        let Some(entry) = entry else {
            return Protection::None;
        };
        match (live_last_played, entry.source_last_played) {
            // Played since the copy was taken.
            (Some(live), Some(saved)) if live > saved => Protection::Stale,
            (Some(_), None) => Protection::Stale,
            _ => Protection::Protected,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    /// Snapshots kept per world; older ones are pruned automatically.
    #[serde(default = "default_retention")]
    pub snapshot_retention: usize,
}

fn default_retention() -> usize {
    5
}

/// Where a world last came from, or last went: the values [`Meta::source`]
/// takes, and the whole set the screen knows how to label.
pub const SOURCE_MINECRAFT: &str = "minecraft";
pub const SOURCE_REALM: &str = "realm";
pub const SOURCE_FILE: &str = "file";
pub const SOURCE_BACKUP: &str = "backup";
pub const SOURCES: [&str; 4] =
    [SOURCE_MINECRAFT, SOURCE_REALM, SOURCE_FILE, SOURCE_BACKUP];

/// Provenance and sync state, stored beside each library world.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Meta {
    /// `minecraftWorlds` folder id this entry mirrors, if it is in the game.
    origin_folder: Option<String>,
    /// Where the world came from: 'local' | 'imported' | 'restored'.
    #[serde(default)]
    origin: String,
    /// Where this copy last came from or last went — see [`Vault::set_source`].
    ///
    /// Not the same question as `origin`, which records how the world first
    /// entered the vault and never changes. Absent on entries written before
    /// it existed: unknown, rather than guessed. Kept as a plain string so an
    /// unrecognised value can never fail the whole file's parse and take the
    /// rest of an entry's metadata down with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    /// `LastPlayed` of the source when the copy was taken; drives staleness.
    source_last_played: Option<i64>,
    /// When the vault copy was last written.
    synced_at: Option<i64>,
    /// Size of the copy, recorded when it was written.
    ///
    /// Cached because listing the library otherwise walks every file of every
    /// world, which makes a refresh after a bulk save look like a hang.
    size_bytes: Option<u64>,
    /// Every `minecraftWorlds` folder id this world has ever occupied.
    ///
    /// Keeps its snapshot history together: archiving clears `origin_folder`,
    /// and activating mints a new id, so without this a world's backups would
    /// scatter into a fresh group each time it moved.
    #[serde(default)]
    past_folders: Vec<String>,
    /// [`fingerprint`] of the world when the last snapshot was taken, so an
    /// unchanged world is not stored again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    snapshot_fingerprint: Option<String>,
    /// `<realm id>/<slot>` this world was last copied down from, so copying
    /// that slot again lands here instead of making a second entry. The same
    /// job `origin_folder` does for a world that lives in the game.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    realm_slot: Option<String>,
}

/// What a re-sync did, so the screen can say which.
#[derive(Debug, Clone)]
pub enum Resync {
    /// The world that arrived was identical; nothing was touched.
    Unchanged(LibraryEntry),
    Replaced(LibraryEntry),
    /// What arrived is a *different* world from the one this entry holds, so
    /// nothing was touched. Carries the entry that was left alone.
    NotTheSameWorld(LibraryEntry),
}

/// What [`Vault::absorb_mcworld`] did with a world that arrived.
#[derive(Debug, Clone)]
pub enum Absorbed {
    /// The vault already held this exact world; nothing was added.
    AlreadyHeld(LibraryEntry),
    /// The world this entry holds had moved on, so its copy was brought up to
    /// date and what it held before was kept as a backup.
    Updated(LibraryEntry),
    Added(LibraryEntry),
}

impl Absorbed {
    pub fn entry(&self) -> &LibraryEntry {
        match self {
            Absorbed::AlreadyHeld(e) | Absorbed::Updated(e) | Absorbed::Added(e) => e,
        }
    }
}

fn realm_slot_key(realm_id: i64, slot: i64) -> String {
    format!("{realm_id}/{slot}")
}

/// A content fingerprint of a world folder: every file's path and bytes.
///
/// Deliberately not a cheap proxy. Sizes and timestamps both miss real edits —
/// a chunk rewritten to the same length, a `level.dat` untouched while the
/// database moved — and the cost of missing one is a lost backup, so this reads
/// the world rather than guessing about it. Reading is still an order of
/// magnitude cheaper than the packing it saves, which is the whole point.
///
/// Empty when anything cannot be read, which callers must treat as "cannot
/// tell" and never as "unchanged".
fn fingerprint(world_dir: &Path) -> String {
    fn feed(hash: &mut u64, bytes: &[u8]) {
        // FNV-1a: a few lines, no dependency, and this only has to notice a
        // difference — nothing here is defending against a forged world.
        for byte in bytes {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
    }

    if !world_dir.join("level.dat").is_file() {
        return String::new();
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut files = 0u64;
    let mut bytes = 0u64;
    // Sorted, so the same world always hashes the same way whatever order the
    // filesystem hands the entries back in.
    for entry in walkdir::WalkDir::new(world_dir).sort_by_file_name() {
        let Ok(entry) = entry else { return String::new() };
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(world_dir) else { return String::new() };
        let Ok(content) = fs::read(entry.path()) else { return String::new() };
        feed(&mut hash, rel.to_string_lossy().replace('\\', "/").as_bytes());
        feed(&mut hash, &content);
        files += 1;
        bytes += content.len() as u64;
    }
    format!("{files}-{bytes}-{hash:016x}")
}

pub struct Vault {
    pub root: PathBuf,
    pub settings: Settings,
}

#[derive(Debug, Clone)]
pub struct LibraryEntry {
    pub id: String,
    pub name: String,
    pub world_dir: PathBuf,
    pub size_bytes: u64,
    pub last_played: Option<i64>,
    pub version: Option<String>,
    pub game_mode: &'static str,
    pub origin_folder: Option<String>,
    pub origin: String,
    /// Where this copy last came from or last went: one of [`SOURCES`], or
    /// `None` for a world last touched before the vault recorded it.
    pub source: Option<String>,
    /// `<realm id>/<slot>` this world was last copied down from.
    pub realm_slot: Option<String>,
    pub synced_at: Option<i64>,
    /// `LastPlayed` of the source when this copy was taken.
    pub source_last_played: Option<i64>,
    /// Every `minecraftWorlds` folder id this world has occupied.
    pub past_folders: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub path: PathBuf,
    /// Timestamp portion of the filename, e.g. `20260810-102406`.
    pub stamp: String,
    pub size_bytes: u64,
}

/// Every snapshot belonging to one world.
#[derive(Debug, Clone)]
pub struct BackupGroup {
    /// Vault id or live folder id the snapshots are filed under.
    pub key: String,
    pub name: String,
    pub snapshots: Vec<Snapshot>,
}

/// What decides that two backup folders hold the same world: its seed and the
/// name stored with the snapshots.
///
/// Seed alone is not enough — every copy of a marketplace map shares one — and
/// name alone is the thing `all_backups` has always refused, since plenty of
/// people have three worlds called "My World". Together they are as close to an
/// identity as a Bedrock world has: nothing in `level.dat` is unique per world.
///
/// The seed is written beside the snapshots when they are taken; older folders
/// pay one archive read and then cache it the same way.
fn world_identity(dirs: &[PathBuf], snapshots: &[Snapshot]) -> Option<(i64, String)> {
    let seed = dirs
        .iter()
        .find_map(|d| fs::read_to_string(d.join("seed.txt")).ok())
        .and_then(|s| s.trim().parse::<i64>().ok())
        .or_else(|| {
            let found = mcworld::seed_in_archive(&snapshots.first()?.path)?;
            if let Some(dir) = dirs.first() {
                let _ = fs::write(dir.join("seed.txt"), found.to_string());
            }
            Some(found)
        })?;
    let name = dirs
        .iter()
        .find_map(|d| read_name_file(d))
        .or_else(|| mcworld::name_in_archive(&snapshots.first()?.path))?;
    Some((seed, name))
}

fn read_name_file(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join("name.txt"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// A world's name, from `level.dat` with `levelname.txt` as the fallback.
pub fn world_display_name(world_dir: &Path) -> Option<String> {
    world_name(world_dir)
}

/// A world's seed: what identifies it when a name cannot be trusted.
fn world_seed(world_dir: &Path) -> Option<i64> {
    fs::read(world_dir.join("level.dat"))
        .ok()
        .and_then(|d| level_dat::parse(&d).ok())
        .and_then(|m| m.seed)
}

/// A world's name, from `level.dat` with `levelname.txt` as the fallback.
fn world_name(world_dir: &Path) -> Option<String> {
    fs::read(world_dir.join("level.dat"))
        .ok()
        .and_then(|d| level_dat::parse(&d).ok())
        .and_then(|m| m.name)
        .or_else(|| {
            fs::read_to_string(world_dir.join("levelname.txt"))
                .ok()
                .map(|s| s.trim().to_owned())
        })
        .filter(|s| !s.is_empty())
}

impl Vault {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        for sub in ["library", "backups", "exports"] {
            fs::create_dir_all(root.join(sub))
                .with_context(|| format!("creating {}", root.join(sub).display()))?;
        }
        let settings = fs::read_to_string(root.join("settings.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| Settings { snapshot_retention: default_retention() });
        Ok(Self { root, settings })
    }

    pub fn library_dir(&self) -> PathBuf {
        self.root.join("library")
    }

    pub fn backups_dir(&self) -> PathBuf {
        self.root.join("backups")
    }

    pub fn exports_dir(&self) -> PathBuf {
        self.root.join("exports")
    }

    pub fn save_settings(&self) -> Result<()> {
        fs::write(
            self.root.join("settings.json"),
            serde_json::to_string_pretty(&self.settings)?,
        )?;
        Ok(())
    }

    fn meta_path(&self, id: &str) -> PathBuf {
        self.library_dir().join(id).join("meta.json")
    }

    fn read_meta(&self, id: &str) -> Meta {
        fs::read_to_string(self.meta_path(id))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn write_meta(&self, id: &str, meta: &Meta) -> Result<()> {
        fs::write(self.meta_path(id), serde_json::to_string_pretty(meta)?)?;
        Ok(())
    }

    /// Every world in the library, newest-played first.
    pub fn list(&self) -> Result<Vec<LibraryEntry>> {
        let dir = self.library_dir();
        let mut out = Vec::new();
        if !dir.is_dir() {
            return Ok(out);
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let id = entry.file_name().to_string_lossy().into_owned();
            let world_dir = entry.path().join("world");
            if !world_dir.join("level.dat").is_file() {
                continue;
            }
            let mut meta = self.read_meta(&id);
            // Entries written before sizes were cached backfill themselves on
            // first read, so only one listing pays for the directory walk.
            if meta.size_bytes.is_none() {
                meta.size_bytes = Some(scan::dir_size(&world_dir));
                let _ = self.write_meta(&id, &meta);
            }
            let level = fs::read(world_dir.join("level.dat"))
                .ok()
                .and_then(|d| level_dat::parse(&d).ok());
            let fallback = fs::read_to_string(world_dir.join("levelname.txt"))
                .ok()
                .map(|s| s.trim().to_owned());
            out.push(LibraryEntry {
                id,
                name: level
                    .as_ref()
                    .and_then(|m| m.name.clone())
                    .or(fallback)
                    .unwrap_or_else(|| "<unnamed>".into()),
                size_bytes: meta.size_bytes.unwrap_or_else(|| scan::dir_size(&world_dir)),
                last_played: level.as_ref().and_then(|m| m.last_played),
                version: level.as_ref().and_then(|m| m.version.clone()),
                game_mode: level.as_ref().map(|m| m.game_mode_label()).unwrap_or("-"),
                origin_folder: meta.origin_folder,
                origin: meta.origin,
                source: meta.source,
                realm_slot: meta.realm_slot,
                synced_at: meta.synced_at,
                source_last_played: meta.source_last_played,
                past_folders: meta.past_folders,
                world_dir,
            });
        }
        out.sort_by_key(|e| std::cmp::Reverse(e.last_played.unwrap_or(i64::MIN)));
        Ok(out)
    }

    pub fn entry(&self, id: &str) -> Result<LibraryEntry> {
        self.list()?
            .into_iter()
            .find(|e| e.id == id)
            .with_context(|| format!("no library world with id '{id}'"))
    }

    /// The library entry mirroring a given `minecraftWorlds` folder, if any.
    pub fn find_by_origin(&self, folder_id: &str) -> Result<Option<LibraryEntry>> {
        Ok(self
            .list()?
            .into_iter()
            .find(|e| e.origin_folder.as_deref() == Some(folder_id)))
    }

    /// Protection state of a live world, given its `LastPlayed`.
    ///
    /// Re-reads the library; when checking many worlds at once, list the
    /// library once and use [`Protection::compare`] instead.
    pub fn protection(&self, folder_id: &str, live_last_played: Option<i64>) -> Result<Protection> {
        let entry = self.find_by_origin(folder_id)?;
        Ok(Protection::compare(entry.as_ref(), live_last_played))
    }

    /// Copy a live world into the vault, or refresh the copy it already has.
    ///
    /// The world stays exactly where it is — this is protection, not archiving.
    pub fn protect(&self, live_world: &Path, folder_id: &str, stamp: &str) -> Result<LibraryEntry> {
        guard::ensure_closed()?;
        if !live_world.join("level.dat").is_file() {
            bail!("{} is not a world folder", live_world.display());
        }
        let live_last_played = fs::read(live_world.join("level.dat"))
            .ok()
            .and_then(|d| level_dat::parse(&d).ok())
            .and_then(|m| m.last_played);

        let existing = self.find_by_origin(folder_id)?;
        let id = match &existing {
            Some(e) => e.id.clone(),
            None => self.new_id(stamp),
        };
        let entry_dir = self.library_dir().join(&id);
        let world_dir = entry_dir.join("world");
        fs::create_dir_all(&entry_dir)?;

        let bytes = if existing.is_some() {
            // Snapshot what the vault currently holds before replacing it, then
            // stage the new copy alongside and swap only once it verifies.
            self.snapshot(&id, &world_dir, stamp)?;
            let staged = entry_dir.join("world.new");
            let _ = fs::remove_dir_all(&staged);
            let (_, bytes) = copy_verified(live_world, &staged).inspect_err(|_| {
                let _ = fs::remove_dir_all(&staged);
            })?;
            fs::remove_dir_all(&world_dir)?;
            fs::rename(&staged, &world_dir)?;
            bytes
        } else {
            let (_, bytes) = copy_verified(live_world, &world_dir).inspect_err(|_| {
                let _ = fs::remove_dir_all(&entry_dir);
            })?;
            self.snapshot(&id, &world_dir, stamp)?;
            bytes
        };

        let mut meta = self.read_meta(&id);
        meta.origin_folder = Some(folder_id.to_owned());
        if !meta.past_folders.iter().any(|f| f == folder_id) {
            meta.past_folders.push(folder_id.to_owned());
        }
        if meta.origin.is_empty() {
            meta.origin = "local".into();
        }
        // Wherever this world came from before, it has just come out of the
        // game — so that is where it was last.
        meta.source = Some(SOURCE_MINECRAFT.to_owned());
        meta.source_last_played = live_last_played;
        meta.synced_at = Some(now());
        meta.size_bytes = Some(bytes);
        self.write_meta(&id, &meta)?;
        self.entry(&id)
    }

    /// Protect a world and remove it from the in-game list.
    pub fn archive(&self, live_world: &Path, folder_id: &str, stamp: &str) -> Result<LibraryEntry> {
        let entry = self.protect(live_world, folder_id, stamp)?;
        fs::remove_dir_all(live_world)
            .with_context(|| format!("removing source {}", live_world.display()))?;
        // The world is no longer in the game, so it mirrors no live folder.
        let mut meta = self.read_meta(&entry.id);
        meta.origin_folder = None;
        self.write_meta(&entry.id, &meta)?;
        self.entry(&entry.id)
    }

    /// Copy a vault world into `minecraftWorlds` under a fresh folder id.
    ///
    /// The library keeps its copy — it is the source of truth — and records the
    /// new folder id so the two stay linked.
    pub fn activate(&self, id: &str, worlds_dir: &Path, stamp: &str) -> Result<PathBuf> {
        guard::ensure_closed()?;
        let entry = self.entry(id)?;
        fs::create_dir_all(worlds_dir)?;
        let folder_id = format!("BV{stamp}");
        let dest = worlds_dir.join(&folder_id);
        if dest.exists() {
            bail!("{} already exists", dest.display());
        }
        copy_verified(&entry.world_dir, &dest).inspect_err(|_| {
            let _ = fs::remove_dir_all(&dest);
        })?;

        let mut meta = self.read_meta(id);
        if !meta.past_folders.contains(&folder_id) {
            meta.past_folders.push(folder_id.clone());
        }
        meta.origin_folder = Some(folder_id);
        meta.source_last_played = entry.last_played;
        // It is in the game now, wherever it came from before.
        meta.source = Some(SOURCE_MINECRAFT.to_owned());
        self.write_meta(id, &meta)?;
        Ok(dest)
    }

    /// Import a world folder into the library.
    pub fn import_folder(&self, world_dir: &Path, stamp: &str) -> Result<LibraryEntry> {
        if !world_dir.join("level.dat").is_file() {
            bail!("{} has no level.dat", world_dir.display());
        }
        let id = self.new_id(stamp);
        let dest = self.library_dir().join(&id).join("world");
        fs::create_dir_all(dest.parent().unwrap())?;
        copy_verified(world_dir, &dest)?;
        self.finish_import(&id, "imported")
    }

    /// Import a `.mcworld` archive into the library.
    pub fn import_mcworld(&self, mcworld: &Path, stamp: &str) -> Result<LibraryEntry> {
        let id = self.new_id(stamp);
        let dest = self.library_dir().join(&id).join("world");
        fs::create_dir_all(dest.parent().unwrap())?;
        mcworld::unpack(mcworld, &dest).inspect_err(|_| {
            let _ = fs::remove_dir_all(self.library_dir().join(&id));
        })?;
        self.finish_import(&id, "imported")
    }

    /// Take a `.mcworld` into the vault without ever storing the same world
    /// twice.
    ///
    /// Every way a world arrives as a file goes through here, because each of
    /// them minted a fresh entry on its own and the vault filled up with
    /// identical worlds — five copies of one, from replacing a Realm slot three
    /// times over.
    ///
    /// Two checks, in order of how much they know:
    ///
    /// 1. `claim` — the Realm slot this world is coming from. The entry that
    ///    claims it is this world's, so it is updated rather than joined.
    /// 2. Identical content, which needs no provenance at all: if the vault
    ///    already holds these exact bytes, nothing is added. A world that
    ///    differs in any way is a world worth keeping, and is added as its own
    ///    entry.
    pub fn absorb_mcworld(
        &self,
        mcworld: &Path,
        stamp: &str,
        claim: Option<(i64, i64)>,
    ) -> Result<Absorbed> {
        // Set when the slot already has a copy in the vault and what arrived
        // was a *different* world. The claim stays where it is: an old occupant
        // of a slot is not the world on it, so it must not inherit the slot.
        let mut claim_is_spoken_for = false;
        if let Some((realm_id, slot)) = claim {
            if let Some(held) = self.find_by_realm_slot(realm_id, slot)? {
                match self.resync_mcworld(&held.id, mcworld, stamp)? {
                    Resync::Unchanged(entry) => return Ok(Absorbed::AlreadyHeld(entry)),
                    Resync::Replaced(entry) => return Ok(Absorbed::Updated(entry)),
                    // Not an update of anything — fall through and let it stand
                    // or be recognised on its own merits.
                    Resync::NotTheSameWorld(_) => claim_is_spoken_for = true,
                }
            }
        }
        let claim = claim.filter(|_| !claim_is_spoken_for);

        // Unpacking is the same work as inspecting the archive would be, so the
        // copy is made and then dropped if it turns out to be one the vault has.
        let fresh = self.import_mcworld(mcworld, stamp)?;
        if let Some(held) = self.find_same_content(&fresh.world_dir, &fresh.id)? {
            self.forget(&fresh.id)?;
            if let Some((realm_id, slot)) = claim {
                self.set_realm_slot(&held.id, realm_id, slot)?;
            }
            return Ok(Absorbed::AlreadyHeld(self.entry(&held.id)?));
        }
        if let Some((realm_id, slot)) = claim {
            self.set_realm_slot(&fresh.id, realm_id, slot)?;
        }
        Ok(Absorbed::Added(self.entry(&fresh.id)?))
    }

    /// Take a `.mcworld` into the entry that already holds this world.
    ///
    /// Copying the same world in twice used to leave two identical entries,
    /// because every import minted a fresh id. Saving from Minecraft has never
    /// behaved that way — it finds the entry by the world's game folder and
    /// re-syncs it — and this is the same idea for a world arriving as a file:
    /// the caller says which entry it belongs to, and the vault updates that
    /// one, keeping what was there as a backup first.
    ///
    /// The staged copy is compared before anything is disturbed, so a repeat of
    /// a world that has not moved on changes nothing at all.
    pub fn resync_mcworld(&self, id: &str, mcworld: &Path, stamp: &str) -> Result<Resync> {
        let entry = self.entry(id)?;

        // The archive a Realm slot hands back is not always that slot's world:
        // the service serves its own stored copy, which stays as it was until
        // somebody plays there, so it can be a world that used to be on the
        // slot. Trusting it replaced one of the player's worlds with a
        // different one — Maia World became Hardcore Mode — so the seed has to
        // agree before anything is written. It is the one thing about a world
        // that cannot be renamed.
        let arriving = mcworld::seed_in_archive(mcworld);
        if arriving.is_none() || arriving != world_seed(&entry.world_dir) {
            return Ok(Resync::NotTheSameWorld(entry));
        }

        let staged = self.library_dir().join(id).join("world.new");
        let _ = fs::remove_dir_all(&staged);
        mcworld::unpack(mcworld, &staged).inspect_err(|_| {
            let _ = fs::remove_dir_all(&staged);
        })?;

        let arriving = fingerprint(&staged);
        let held = fingerprint(&entry.world_dir);
        if !arriving.is_empty() && arriving == held {
            let _ = fs::remove_dir_all(&staged);
            return Ok(Resync::Unchanged(entry));
        }

        // Keep what is being replaced, exactly as a re-save from the game does.
        self.snapshot(id, &entry.world_dir, stamp)?;
        fs::remove_dir_all(&entry.world_dir)?;
        fs::rename(&staged, &entry.world_dir)?;

        let mut meta = self.read_meta(id);
        meta.synced_at = Some(now());
        meta.size_bytes = Some(scan::dir_size(&entry.world_dir));
        self.write_meta(id, &meta)?;
        Ok(Resync::Replaced(self.entry(id)?))
    }

    /// The entry holding the world last copied down from this Realm slot.
    ///
    /// Provenance, not guesswork: a re-download of the same slot lands on the
    /// entry it made last time, and a download from anywhere else never does.
    /// Worlds that merely look alike — two playthroughs of one marketplace map,
    /// sharing a seed and a name — are left well alone.
    pub fn find_by_realm_slot(&self, realm_id: i64, slot: i64) -> Result<Option<LibraryEntry>> {
        let want = realm_slot_key(realm_id, slot);
        Ok(self
            .list()?
            .into_iter()
            .find(|e| e.realm_slot.as_deref() == Some(want.as_str())))
    }

    /// Record that this entry is the vault's copy of a Realm slot.
    ///
    /// Exactly one entry may claim a slot, so the claim moves rather than being
    /// shared: sending a different world up to a slot makes *that* world the
    /// one the slot holds, and the world that used to be there is no longer a
    /// copy of it.
    pub fn set_realm_slot(&self, id: &str, realm_id: i64, slot: i64) -> Result<()> {
        let key = realm_slot_key(realm_id, slot);
        for other in self.list()? {
            if other.id != id && other.realm_slot.as_deref() == Some(key.as_str()) {
                let mut meta = self.read_meta(&other.id);
                meta.realm_slot = None;
                self.write_meta(&other.id, &meta)?;
            }
        }
        let mut meta = self.read_meta(id);
        meta.realm_slot = Some(key);
        self.write_meta(id, &meta)
    }

    /// An entry whose world is byte-for-byte what is in this folder.
    ///
    /// For a world arriving as a file, where there is no provenance to go on:
    /// identical content is the one claim that needs no judgement. Name and
    /// size narrow the field first so this fingerprints one or two worlds
    /// rather than the whole library. `except` is the entry being checked,
    /// which would otherwise match itself.
    pub fn find_same_content(
        &self,
        world_dir: &Path,
        except: &str,
    ) -> Result<Option<LibraryEntry>> {
        let arriving = fingerprint(world_dir);
        if arriving.is_empty() {
            return Ok(None);
        }
        let name = world_name(world_dir);
        let (_, bytes) = count_and_size(world_dir);
        Ok(self
            .list()?
            .into_iter()
            .filter(|e| e.id != except && Some(&e.name) == name.as_ref() && e.size_bytes == bytes)
            .find(|e| fingerprint(&e.world_dir) == arriving))
    }

    /// Remove a library entry, keeping nothing.
    ///
    /// For a copy that should not have been made — an import of a world the
    /// vault already holds. [`Vault::delete`] is the one that leaves a final
    /// snapshot behind; this deliberately is not, because the world it is
    /// removing still exists under another id.
    pub fn forget(&self, id: &str) -> Result<()> {
        if id.is_empty() || Path::new(id).components().count() != 1 {
            bail!("'{id}' is not a vault id");
        }
        let dir = self.library_dir().join(id);
        if !dir.join("world").join("level.dat").is_file() {
            bail!("no vault world '{id}'");
        }
        fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
        Ok(())
    }

    /// Rebuild a world from a snapshot as a **new** library entry.
    ///
    /// Non-destructive by design: rolling back never overwrites the current
    /// copy, so a mistaken restore costs nothing.
    pub fn restore_snapshot(&self, snapshot: &Path, stamp: &str) -> Result<LibraryEntry> {
        let id = self.new_id(stamp);
        let dest = self.library_dir().join(&id).join("world");
        fs::create_dir_all(dest.parent().unwrap())?;
        mcworld::unpack(snapshot, &dest).inspect_err(|_| {
            let _ = fs::remove_dir_all(self.library_dir().join(&id));
        })?;
        self.finish_import(&id, "restored")
    }

    fn finish_import(&self, id: &str, origin: &str) -> Result<LibraryEntry> {
        let entry = self.entry(id)?;
        let meta = Meta {
            origin_folder: None,
            source: Some(
                match origin {
                    "restored" => SOURCE_BACKUP,
                    _ => SOURCE_FILE,
                }
                .to_owned(),
            ),
            origin: origin.to_owned(),
            source_last_played: entry.last_played,
            synced_at: Some(now()),
            size_bytes: Some(scan::dir_size(&entry.world_dir)),
            past_folders: Vec::new(),
            snapshot_fingerprint: None,
            realm_slot: None,
        };
        self.write_meta(id, &meta)?;
        self.entry(id)
    }

    /// Record where a world last came from, or last went.
    ///
    /// Everything a world can arrive from is already known to the vault except
    /// a Realm, which the caller holding the session has to say — and it is
    /// also the one place a world *goes* to, so sending one up sets it too.
    pub fn set_source(&self, id: &str, source: &str) -> Result<()> {
        let mut meta = self.read_meta(id);
        meta.source = Some(source.to_owned());
        self.write_meta(id, &meta)
    }

    /// Remove a world from the library, leaving a final snapshot behind.
    pub fn delete(&self, id: &str, stamp: &str) -> Result<PathBuf> {
        let entry = self.entry(id)?;
        let backup = self.snapshot_named(id, &entry.world_dir, &format!("{stamp}-deleted"))?;
        fs::remove_dir_all(self.library_dir().join(id))?;
        Ok(backup)
    }

    /// Take a snapshot and prune old ones per the retention setting.
    ///
    /// `None` when the world is unchanged since the last snapshot: a second
    /// copy of identical bytes is not history, it is just disk. One world here
    /// held three 15 MB archives of the same unplayed world.
    pub fn snapshot(&self, id: &str, world_dir: &Path, stamp: &str) -> Result<Option<PathBuf>> {
        let now = fingerprint(world_dir);
        if let Some(last) = self.read_meta(id).snapshot_fingerprint {
            // Only skip on a positive match. An unreadable world fingerprints
            // as nothing, and "cannot tell" must always mean "take the copy".
            if !now.is_empty() && last == now && !self.snapshots_for_key(id).is_empty() {
                return Ok(None);
            }
        }

        let path = self.snapshot_named(id, world_dir, stamp)?;
        let mut meta = self.read_meta(id);
        meta.snapshot_fingerprint = Some(now);
        self.write_meta(id, &meta)?;
        self.prune_snapshots(id)?;
        Ok(Some(path))
    }

    fn snapshot_named(&self, id: &str, world_dir: &Path, stamp: &str) -> Result<PathBuf> {
        let dir = self.backups_dir().join(id);
        fs::create_dir_all(&dir)?;
        let out = dir.join(format!("{stamp}.mcworld"));
        mcworld::pack(world_dir, &out)?;
        // Remember the world's name so backups stay identifiable after the
        // world itself is deleted from the vault, and its seed so copies of the
        // same world can be recognised without opening an archive.
        if let Some(name) = world_name(world_dir) {
            let _ = fs::write(dir.join("name.txt"), name);
        }
        if let Some(seed) = fs::read(world_dir.join("level.dat"))
            .ok()
            .and_then(|d| level_dat::parse(&d).ok())
            .and_then(|m| m.seed)
        {
            let _ = fs::write(dir.join("seed.txt"), seed.to_string());
        }
        Ok(out)
    }

    /// All snapshots on disk, grouped per world, newest group first.
    ///
    /// `library` supplies current names; pass the list you already have.
    ///
    /// Snapshots of one world can be filed under two keys — Minecraft's folder
    /// id for those taken before it was saved to the vault, and the vault id
    /// after — so those are merged. Merging is by that explicit link and never
    /// by name, since a player can easily have several worlds called
    /// "My World" and combining their histories would be a lie.
    pub fn all_backups(&self, library: &[LibraryEntry]) -> Vec<BackupGroup> {
        let Ok(entries) = fs::read_dir(self.backups_dir()) else {
            return Vec::new();
        };
        let names: std::collections::HashMap<&str, &str> =
            library.iter().map(|e| (e.id.as_str(), e.name.as_str())).collect();
        let alias: std::collections::HashMap<&str, &str> = library
            .iter()
            .flat_map(|e| {
                e.past_folders
                    .iter()
                    .map(String::as_str)
                    .chain(e.origin_folder.as_deref())
                    .map(move |folder| (folder, e.id.as_str()))
            })
            .collect();

        // group key -> (snapshots, directories they came from)
        let mut merged: std::collections::HashMap<String, (Vec<Snapshot>, Vec<PathBuf>)> =
            std::collections::HashMap::new();
        for entry in entries.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()) {
            let key = entry.file_name().to_string_lossy().into_owned();
            let snapshots = self.snapshots_for_key(&key);
            if snapshots.is_empty() {
                continue;
            }
            let group_key = alias.get(key.as_str()).map(|s| (*s).to_owned()).unwrap_or(key);
            let slot = merged.entry(group_key).or_default();
            slot.0.extend(snapshots);
            slot.1.push(entry.path());
        }

        // (identity, group) — identity is what decides whether two vault ids
        // are really the same world; see `world_identity`.
        let mut groups: Vec<(Option<(i64, String)>, BackupGroup)> = merged
            .into_iter()
            .map(|(key, (mut snapshots, dirs))| {
                snapshots.sort_by(|a, b| b.stamp.cmp(&a.stamp));
                let name = names
                    .get(key.as_str())
                    .map(|s| (*s).to_owned())
                    .or_else(|| dirs.iter().find_map(|d| read_name_file(d)))
                    .or_else(|| {
                        // Snapshots taken before names were recorded are filed
                        // under Minecraft's own folder id (`9Ysgap55yI8=`),
                        // which is meaningless on screen. Read the name out of
                        // the newest snapshot and cache it so this happens once.
                        let found = snapshots
                            .first()
                            .and_then(|s| mcworld::name_in_archive(&s.path));
                        if let (Some(name), Some(dir)) = (&found, dirs.first()) {
                            let _ = fs::write(dir.join("name.txt"), name);
                        }
                        found
                    })
                    .unwrap_or_else(|| "Unknown world".to_owned());
                let identity = world_identity(&dirs, &snapshots);
                (identity, BackupGroup { key, name, snapshots })
            })
            .collect();

        groups.sort_by(|a, b| {
            let newest = |g: &BackupGroup| {
                g.snapshots.first().map(|s| s.stamp.clone()).unwrap_or_default()
            };
            newest(&b.1).cmp(&newest(&a.1))
        });

        // Fold together the groups holding the same world. One world can end up
        // under several vault ids — every import and every Realm download mints
        // a new one — which showed the same world as three separate sections.
        // The newest group's key and name win, since that is the copy the
        // player last saw.
        //
        // Lineage vetoes a merge: a world that has lived in its own
        // `minecraftWorlds` folder is a world the game itself keeps separate,
        // so two groups with folders that do not overlap stay apart however
        // alike they look. That is what tells two worlds made from the same
        // marketplace map — same seed, same name — from two copies of one.
        let folders: std::collections::HashMap<&str, std::collections::HashSet<&str>> = library
            .iter()
            .map(|e| {
                let mut set: std::collections::HashSet<&str> =
                    e.past_folders.iter().map(String::as_str).collect();
                set.extend(e.origin_folder.as_deref());
                (e.id.as_str(), set)
            })
            .collect();

        let mut out: Vec<BackupGroup> = Vec::with_capacity(groups.len());
        let mut seen: Vec<((i64, String), std::collections::HashSet<&str>, usize)> = Vec::new();
        for (identity, group) in groups {
            let lineage = folders.get(group.key.as_str()).cloned().unwrap_or_default();
            let existing = identity.as_ref().and_then(|id| {
                seen.iter().position(|(seen_id, seen_folders, _)| {
                    seen_id == id
                        && (lineage.is_empty()
                            || seen_folders.is_empty()
                            || !seen_folders.is_disjoint(&lineage))
                })
            });
            match existing {
                Some(slot) => {
                    let (_, seen_folders, at) = &mut seen[slot];
                    seen_folders.extend(lineage);
                    let into: &mut BackupGroup = &mut out[*at];
                    into.snapshots.extend(group.snapshots);
                    into.snapshots.sort_by(|a, b| b.stamp.cmp(&a.stamp));
                }
                None => {
                    if let Some(id) = identity {
                        seen.push((id, lineage, out.len()));
                    }
                    out.push(group);
                }
            }
        }
        out
    }

    /// Snapshot history for an entry, newest first.
    ///
    /// Also picks up snapshots filed under every `minecraftWorlds` folder the
    /// world has occupied, so history taken before it was saved — or under an
    /// earlier folder id — is not orphaned.
    pub fn snapshots(&self, entry: &LibraryEntry) -> Vec<Snapshot> {
        let mut keys = vec![entry.id.clone()];
        keys.extend(entry.past_folders.iter().cloned());
        if let Some(folder) = &entry.origin_folder {
            keys.push(folder.clone());
        }
        keys.sort();
        keys.dedup();

        let mut out = Vec::new();
        for key in keys {
            out.extend(self.snapshots_for_key(&key));
        }
        out.sort_by(|a, b| b.stamp.cmp(&a.stamp));
        out
    }

    /// Snapshots filed under any key (vault id or live folder id).
    pub fn snapshots_for_key(&self, key: &str) -> Vec<Snapshot> {
        let dir = self.backups_dir().join(key);
        let Ok(entries) = fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out: Vec<Snapshot> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "mcworld"))
            .map(|e| Snapshot {
                stamp: e.path().file_stem().unwrap_or_default().to_string_lossy().into_owned(),
                size_bytes: e.metadata().map(|m| m.len()).unwrap_or(0),
                path: e.path(),
            })
            .collect();
        out.sort_by(|a, b| b.stamp.cmp(&a.stamp));
        out
    }

    /// Delete one backup for good.
    ///
    /// The path arrives from the screen, so it is checked against this vault's
    /// own backups folder first: nothing outside it, and nothing that is not a
    /// snapshot, can be removed through here. Unlike deleting a world, there is
    /// no copy left behind — this *is* the copy.
    pub fn delete_snapshot(&self, snapshot: &Path) -> Result<()> {
        let backups = self
            .backups_dir()
            .canonicalize()
            .with_context(|| format!("reading {}", self.backups_dir().display()))?;
        let target = snapshot
            .canonicalize()
            .with_context(|| format!("that backup is no longer at {}", snapshot.display()))?;
        if !target.starts_with(&backups)
            || target.extension().is_none_or(|x| x != "mcworld")
            || !target.is_file()
        {
            bail!("{} is not a backup in this vault", snapshot.display());
        }
        fs::remove_file(&target).with_context(|| format!("removing {}", target.display()))?;

        // Once the last snapshot of a world is gone the folder holds only the
        // remembered name, which is no use on its own.
        if let Some(dir) = target.parent().filter(|d| *d != backups) {
            let empty = fs::read_dir(dir).map(|mut entries| {
                !entries.any(|e| {
                    e.is_ok_and(|e| e.path().extension().is_some_and(|x| x == "mcworld"))
                })
            });
            if empty.unwrap_or(false) {
                let _ = fs::remove_dir_all(dir);
            }
        }
        Ok(())
    }

    fn prune_snapshots(&self, id: &str) -> Result<()> {
        let keep = self.settings.snapshot_retention.max(1);
        let snaps = self.snapshots_for_key(id);
        // Deletion snapshots are the last copy of a removed world; never prune.
        for snap in snaps.iter().filter(|s| !s.stamp.ends_with("-deleted")).skip(keep) {
            let _ = fs::remove_file(&snap.path);
        }
        Ok(())
    }

    /// Vault ids are timestamp-based: sortable, readable, and unique because a
    /// second operation in the same second gets a numeric suffix.
    fn new_id(&self, stamp: &str) -> String {
        let base = format!("w{stamp}");
        if !self.library_dir().join(&base).exists() {
            return base;
        }
        (2..)
            .map(|n| format!("{base}-{n}"))
            .find(|c| !self.library_dir().join(c).exists())
            .unwrap()
    }
}

/// Whether a folder already holds a vault.
pub fn looks_like_vault(root: &Path) -> bool {
    root.join("library").is_dir()
}

/// Whether a folder is empty (or absent), and so safe to move a vault into.
pub fn is_empty_dir(root: &Path) -> bool {
    match fs::read_dir(root) {
        Ok(mut entries) => entries.next().is_none(),
        // A path that does not exist yet counts as empty.
        Err(_) => !root.exists(),
    }
}

/// Move an entire vault to a new location.
///
/// Copies everything, verifies it, and only then removes the original — so an
/// interrupted move leaves the old vault intact. `on_file` reports progress.
pub fn move_vault(from: &Path, to: &Path, mut on_file: impl FnMut(u64, u64)) -> Result<u64> {
    if !looks_like_vault(from) {
        bail!("{} does not look like a vault", from.display());
    }
    if from == to {
        bail!("the vault is already there");
    }
    if !is_empty_dir(to) {
        bail!("{} is not empty", to.display());
    }

    let total: u64 = walkdir::WalkDir::new(from)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .count() as u64;

    fs::create_dir_all(to)?;
    let mut done = 0u64;
    for entry in walkdir::WalkDir::new(from) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(from).unwrap();
        let target = to.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target)
                .with_context(|| format!("copying {}", entry.path().display()))?;
            done += 1;
            on_file(done, total);
        }
    }

    let (moved_files, _) = count_and_size(to);
    let (source_files, _) = count_and_size(from);
    if moved_files != source_files {
        bail!(
            "move verification failed: {source_files} files at the source, \
             {moved_files} at the destination — the original has been left alone"
        );
    }
    fs::remove_dir_all(from).with_context(|| format!("removing {}", from.display()))?;
    Ok(done)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Recursive copy that verifies file count and total size before returning.
fn copy_verified(src: &Path, dest: &Path) -> Result<(u64, u64)> {
    fs::create_dir_all(dest)?;
    let mut files = 0u64;
    let mut bytes = 0u64;
    for entry in walkdir::WalkDir::new(src) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(src).unwrap();
        let target = dest.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            // Locked files abort the whole operation; never skip silently.
            let n = fs::copy(entry.path(), &target)
                .with_context(|| format!("copying {}", entry.path().display()))?;
            files += 1;
            bytes += n;
        }
    }

    let (dest_files, dest_bytes) = count_and_size(dest);
    if dest_files != files || dest_bytes != bytes {
        bail!(
            "copy verification failed: wrote {files} files/{bytes} bytes, \
             destination has {dest_files} files/{dest_bytes} bytes"
        );
    }
    Ok((files, bytes))
}

fn count_and_size(path: &Path) -> (u64, u64) {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .fold((0, 0), |(c, s), m| (c + 1, s + m.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level_dat::test_fixtures;

    fn make_world(dir: &Path, name: &str) {
        fs::create_dir_all(dir.join("db")).unwrap();
        fs::write(dir.join("level.dat"), test_fixtures::synthetic_level_dat()).unwrap();
        fs::write(dir.join("levelname.txt"), name).unwrap();
        fs::write(dir.join("db").join("CURRENT"), b"MANIFEST-000001\n").unwrap();
    }

    fn temp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("bv-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn protect_keeps_world_in_game_and_marks_protected() {
        let base = temp("protect");
        let live = base.join("minecraftWorlds").join("abc=");
        make_world(&live, "Spike Test World");
        let vault = Vault::open(base.join("vault")).unwrap();

        assert_eq!(vault.protection("abc=", Some(1754000000)).unwrap(), Protection::None);

        let entry = vault.protect(&live, "abc=", "20260810-1200").unwrap();
        assert_eq!(entry.name, "Spike Test World");
        assert!(live.join("level.dat").is_file(), "protect must not move the world");
        assert_eq!(
            vault.protection("abc=", Some(1754000000)).unwrap(),
            Protection::Protected
        );
        // Played since the copy was taken.
        assert_eq!(
            vault.protection("abc=", Some(1754999999)).unwrap(),
            Protection::Stale
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn resync_replaces_copy_and_keeps_one_entry() {
        let base = temp("resync");
        let live = base.join("minecraftWorlds").join("abc=");
        make_world(&live, "Spike Test World");
        let vault = Vault::open(base.join("vault")).unwrap();

        let first = vault.protect(&live, "abc=", "20260810-1200").unwrap();
        fs::write(live.join("db").join("CURRENT"), b"MANIFEST-000002\n").unwrap();
        let second = vault.protect(&live, "abc=", "20260810-1300").unwrap();

        assert_eq!(first.id, second.id, "re-sync must reuse the entry");
        assert_eq!(vault.list().unwrap().len(), 1, "no duplicate entries");
        assert_eq!(
            fs::read(second.world_dir.join("db").join("CURRENT")).unwrap(),
            b"MANIFEST-000002\n",
            "vault copy should hold the newer data"
        );
        // The pre-sync state is in history — held by the snapshot the first
        // save took, since the vault copy did not change in between. Checked by
        // reading it back rather than by counting files, because a second copy
        // of those same bytes would be storage, not history.
        let snapshots = vault.snapshots(&second);
        assert_eq!(snapshots.len(), 1, "no duplicate of an unchanged copy");
        let restored = base.join("readback");
        mcworld::unpack(&snapshots[0].path, &restored).unwrap();
        assert_eq!(
            fs::read(restored.join("db").join("CURRENT")).unwrap(),
            b"MANIFEST-000001\n",
            "history keeps the pre-sync world"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn archive_then_activate_round_trip() {
        let base = temp("vault");
        let live = base.join("minecraftWorlds").join("abc=");
        make_world(&live, "Spike Test World");
        let vault = Vault::open(base.join("vault")).unwrap();

        let entry = vault.archive(&live, "abc=", "20260810-1200").unwrap();
        assert!(!live.exists(), "archive removes the world from the game");
        assert!(entry.origin_folder.is_none());
        assert_eq!(vault.list().unwrap().len(), 1);

        let worlds_dir = base.join("minecraftWorlds");
        let activated = vault.activate(&entry.id, &worlds_dir, "20260810-1300").unwrap();
        assert!(activated.join("level.dat").is_file());
        assert_eq!(
            fs::read(activated.join("db").join("CURRENT")).unwrap(),
            b"MANIFEST-000001\n"
        );
        // Library keeps its copy and relinks to the new live folder.
        assert_eq!(vault.list().unwrap().len(), 1);
        let folder = activated.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(vault.protection(&folder, Some(1754000000)).unwrap(), Protection::Protected);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn delete_keeps_a_final_snapshot_that_restores() {
        let base = temp("delete");
        let live = base.join("w");
        make_world(&live, "Spike Test World");
        let vault = Vault::open(base.join("vault")).unwrap();
        let entry = vault.protect(&live, "abc=", "20260810-1400").unwrap();

        let backup = vault.delete(&entry.id, "20260810-1500").unwrap();
        assert!(backup.is_file(), "delete must leave a snapshot behind");
        assert!(vault.list().unwrap().is_empty());

        let restored = vault.restore_snapshot(&backup, "20260810-1600").unwrap();
        assert_eq!(restored.name, "Spike Test World");
        assert_eq!(restored.origin, "restored");
        assert_eq!(vault.list().unwrap().len(), 1);

        let _ = fs::remove_dir_all(&base);
    }

    /// The tag follows the world about: it says where it last was, not how it
    /// first arrived, so re-saving from the game overwrites "realm".
    #[test]
    fn source_follows_where_the_world_last_was() {
        let base = temp("source");
        let live = base.join("minecraftWorlds").join("abc=");
        make_world(&live, "Spike Test World");
        let vault = Vault::open(base.join("vault")).unwrap();

        let entry = vault.protect(&live, "abc=", "20260811-100000").unwrap();
        assert_eq!(entry.source.as_deref(), Some(SOURCE_MINECRAFT));

        // A Realm download is an import as far as the vault is concerned, so
        // the caller with the session has to say where it really came from.
        vault.set_source(&entry.id, SOURCE_REALM).unwrap();
        assert_eq!(vault.entry(&entry.id).unwrap().source.as_deref(), Some(SOURCE_REALM));

        // Saved out of the game again: it was last in Minecraft.
        let resaved = vault.protect(&live, "abc=", "20260811-110000").unwrap();
        assert_eq!(resaved.id, entry.id, "the same world, not a second entry");
        assert_eq!(resaved.source.as_deref(), Some(SOURCE_MINECRAFT));

        let mcworld = base.join("shared.mcworld");
        mcworld::pack(&entry.world_dir, &mcworld).unwrap();
        let imported = vault.import_mcworld(&mcworld, "20260811-120000").unwrap();
        assert_eq!(imported.source.as_deref(), Some(SOURCE_FILE));

        let snapshot = vault.snapshots(&entry).first().unwrap().path.clone();
        let restored = vault.restore_snapshot(&snapshot, "20260811-130000").unwrap();
        assert_eq!(restored.source.as_deref(), Some(SOURCE_BACKUP));

        // A world saved before the vault recorded any of this stays unknown
        // rather than being labelled with a guess.
        let old = vault.library_dir().join(&entry.id).join("meta.json");
        fs::write(&old, r#"{"origin":"local","past_folders":[]}"#).unwrap();
        assert_eq!(vault.entry(&entry.id).unwrap().source, None);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn deleting_a_backup_removes_that_file_and_nothing_else() {
        let base = temp("delbackup");
        let live = base.join("w");
        make_world(&live, "Spike Test World");
        let vault = Vault::open(base.join("vault")).unwrap();
        let entry = vault.protect(&live, "abc=", "20260811-100000").unwrap();
        fs::write(entry.world_dir.join("db").join("CURRENT"), b"MANIFEST-000002\n").unwrap();
        vault.snapshot(&entry.id, &entry.world_dir, "20260811-110000").unwrap();

        let snaps = vault.snapshots_for_key(&entry.id);
        assert_eq!(snaps.len(), 2);
        vault.delete_snapshot(&snaps[0].path).unwrap();
        let left = vault.snapshots_for_key(&entry.id);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].stamp, snaps[1].stamp);
        assert!(vault.list().unwrap().len() == 1, "the world itself is untouched");

        // Anything outside the vault's backups folder is refused, so a path
        // coming from the screen can never reach the world itself.
        assert!(vault.delete_snapshot(&entry.world_dir.join("level.dat")).is_err());
        assert!(entry.world_dir.join("level.dat").is_file());

        // The last one out takes the folder with it.
        vault.delete_snapshot(&left[0].path).unwrap();
        assert!(!vault.backups_dir().join(&entry.id).exists());

        let _ = fs::remove_dir_all(&base);
    }

    /// The reported case: three 15 MB archives of the same unplayed world.
    #[test]
    fn an_unchanged_world_is_not_stored_again() {
        let base = temp("nodupe");
        let live = base.join("minecraftWorlds").join("abc=");
        make_world(&live, "Spike Test World");
        let vault = Vault::open(base.join("vault")).unwrap();

        let entry = vault.protect(&live, "abc=", "20260811-100000").unwrap();
        assert_eq!(vault.snapshots_for_key(&entry.id).len(), 1);

        // Saving and backing up again, with nothing touched in between.
        assert!(vault.snapshot(&entry.id, &entry.world_dir, "20260811-110000").unwrap().is_none());
        vault.protect(&live, "abc=", "20260811-120000").unwrap();
        assert_eq!(
            vault.snapshots_for_key(&entry.id).len(),
            1,
            "the same bytes must not be stored twice"
        );

        // Play it, and the copy is worth keeping again. Only `level.dat`
        // changes here, and every file keeps its length — the case a
        // size-and-count fingerprint would miss.
        let played = level_dat::test_fixtures::synthetic_level_dat_with_last_played(1754999999);
        assert_eq!(
            played.len(),
            fs::read(entry.world_dir.join("level.dat")).unwrap().len(),
            "the fixture must differ in content but not in size"
        );
        fs::write(entry.world_dir.join("level.dat"), &played).unwrap();
        assert!(vault.snapshot(&entry.id, &entry.world_dir, "20260811-130000").unwrap().is_some());
        assert_eq!(vault.snapshots_for_key(&entry.id).len(), 2);

        // A world whose folder cannot be read must never read as "unchanged".
        assert!(fingerprint(&base.join("nowhere")).is_empty());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn retention_prunes_old_snapshots_but_keeps_deletions() {
        let base = temp("retention");
        let live = base.join("w");
        make_world(&live, "Spike Test World");
        let mut vault = Vault::open(base.join("vault")).unwrap();
        vault.settings.snapshot_retention = 2;

        let entry = vault.protect(&live, "abc=", "20260810-100000").unwrap();
        // The world has to actually change between saves, or the vault rightly
        // refuses to store the same bytes again and there is nothing to prune.
        for (n, stamp) in ["20260810-110000", "20260810-120000", "20260810-130000"]
            .iter()
            .enumerate()
        {
            fs::write(entry.world_dir.join("db").join("CURRENT"), format!("MANIFEST-{n}\n"))
                .unwrap();
            vault.snapshot(&entry.id, &entry.world_dir, stamp).unwrap();
        }
        assert_eq!(vault.snapshots_for_key(&entry.id).len(), 2);

        vault.snapshot_named(&entry.id, &entry.world_dir, "20260810-140000-deleted").unwrap();
        fs::write(entry.world_dir.join("db").join("CURRENT"), b"MANIFEST-later\n").unwrap();
        vault.snapshot(&entry.id, &entry.world_dir, "20260810-150000").unwrap();
        let stamps: Vec<_> = vault.snapshots_for_key(&entry.id).into_iter().map(|s| s.stamp).collect();
        assert!(
            stamps.iter().any(|s| s.ends_with("-deleted")),
            "deletion snapshots survive pruning: {stamps:?}"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// Snapshots made before names were recorded are filed under Minecraft's
    /// folder id; showing that id on screen is meaningless to a player.
    #[test]
    fn legacy_backups_get_their_name_from_the_snapshot() {
        let base = temp("legacybackup");
        let live = base.join("w");
        make_world(&live, "Spike Test World");
        let vault = Vault::open(base.join("vault")).unwrap();

        // Mimic the old layout: keyed by folder id, with no name.txt.
        let key = "9Ysgap55yI8=";
        let dir = vault.backups_dir().join(key);
        fs::create_dir_all(&dir).unwrap();
        mcworld::pack(&live, &dir.join("20260810-090000.mcworld")).unwrap();

        let groups = vault.all_backups(&[]);
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0].name, "Spike Test World",
            "the folder id must never be shown as a world name"
        );
        // And it is cached, so the archive is only read once.
        assert_eq!(
            fs::read_to_string(dir.join("name.txt")).unwrap().trim(),
            "Spike Test World"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// Before and after a world was saved to the vault, its snapshots land
    /// under different keys; the player should still see one history.
    #[test]
    fn snapshots_from_before_and_after_saving_become_one_group() {
        let base = temp("mergebackups");
        let live = base.join("minecraftWorlds").join("9Ysgap55yI8=");
        make_world(&live, "Spike Test World");
        let vault = Vault::open(base.join("vault")).unwrap();

        // A snapshot taken while the world was only in Minecraft.
        let legacy = vault.backups_dir().join("9Ysgap55yI8=");
        fs::create_dir_all(&legacy).unwrap();
        mcworld::pack(&live, &legacy.join("20260810-090000.mcworld")).unwrap();

        // Then it gets saved, which snapshots again under the vault id.
        let entry = vault.protect(&live, "9Ysgap55yI8=", "20260810-100000").unwrap();

        let groups = vault.all_backups(&vault.list().unwrap());
        assert_eq!(groups.len(), 1, "one world should mean one group: {groups:#?}");
        assert_eq!(groups[0].key, entry.id);
        assert_eq!(groups[0].name, "Spike Test World");
        assert_eq!(groups[0].snapshots.len(), 2, "both snapshots are listed");

        // Archiving clears the live link; the history must survive it.
        let archived = vault.archive(&live, "9Ysgap55yI8=", "20260810-110000").unwrap();
        let after = vault.all_backups(&vault.list().unwrap());
        assert_eq!(after.len(), 1, "archiving must not split the history: {after:#?}");
        // Two, not three: archiving re-saved a world that had not changed, and
        // the copy of it already in history is the same bytes.
        assert_eq!(vault.snapshots(&archived).len(), 2);

        let _ = fs::remove_dir_all(&base);
    }

    /// The reported case: copying one Realm slot twice left two identical
    /// worlds in the vault.
    #[test]
    fn copying_a_realm_slot_again_updates_the_world_it_made() {
        let base = temp("realmresync");
        let live = base.join("w");
        make_world(&live, "Hardcore Mode");
        let vault = Vault::open(base.join("vault")).unwrap();

        let download = base.join("slot.mcworld");
        mcworld::pack(&live, &download).unwrap();
        let first = vault.import_mcworld(&download, "20260811-131714").unwrap();
        vault.set_realm_slot(&first.id, 34391948, 2).unwrap();

        // The same slot again, nothing played in between.
        let held = vault.find_by_realm_slot(34391948, 2).unwrap().expect("the slot's world");
        assert_eq!(held.id, first.id);
        match vault.resync_mcworld(&held.id, &download, "20260811-134109").unwrap() {
            Resync::Unchanged(entry) => assert_eq!(entry.id, first.id),
            other => panic!("an identical world must change nothing: {other:?}"),
        }
        assert_eq!(vault.list().unwrap().len(), 1, "no second copy of one world");
        assert!(
            vault.snapshots_for_key(&first.id).is_empty(),
            "and nothing worth backing up happened"
        );

        // Played on the Realm since: the same entry moves on, and what it held
        // is kept.
        fs::write(live.join("db").join("CURRENT"), b"MANIFEST-000002\n").unwrap();
        let newer = base.join("slot-later.mcworld");
        mcworld::pack(&live, &newer).unwrap();
        match vault.resync_mcworld(&first.id, &newer, "20260811-140000").unwrap() {
            Resync::Replaced(entry) => assert_eq!(entry.id, first.id),
            other => panic!("a changed world must replace the copy: {other:?}"),
        }
        assert_eq!(vault.list().unwrap().len(), 1);
        assert_eq!(
            fs::read(first.world_dir.join("db").join("CURRENT")).unwrap(),
            b"MANIFEST-000002\n"
        );
        assert_eq!(
            vault.snapshots_for_key(&first.id).len(),
            1,
            "the copy that was replaced is kept as a backup"
        );

        // A different slot is a different world, however alike it looks.
        assert!(vault.find_by_realm_slot(34391948, 3).unwrap().is_none());

        // Sending a different world up to that slot moves the claim, so only
        // ever one world is the vault's copy of a slot.
        let other = vault.import_mcworld(&newer, "20260811-150000").unwrap();
        vault.set_realm_slot(&other.id, 34391948, 2).unwrap();
        assert_eq!(vault.find_by_realm_slot(34391948, 2).unwrap().map(|e| e.id), Some(other.id));
        assert_eq!(vault.entry(&first.id).unwrap().realm_slot, None);

        let _ = fs::remove_dir_all(&base);
    }

    /// The reported case: swapping a Realm's world three times left five
    /// identical copies of one world in the vault. Each replacement saved the
    /// slot's current world first, and the service kept handing back the same
    /// archive — its stored copy of a slot only changes when somebody plays
    /// there — so every swap imported a world the vault already had.
    #[test]
    fn absorbing_the_same_world_repeatedly_adds_it_once() {
        let base = temp("absorb");
        let live = base.join("w");
        make_world(&live, "Hardcore Mode");
        let vault = Vault::open(base.join("vault")).unwrap();
        let download = base.join("slot.mcworld");
        mcworld::pack(&live, &download).unwrap();

        // No claim recorded — how the copies in the wild were made.
        let first = vault.absorb_mcworld(&download, "20260811-140012", None).unwrap();
        assert!(matches!(first, Absorbed::Added(_)));
        for stamp in ["20260811-140053", "20260811-140118", "20260811-140200"] {
            match vault.absorb_mcworld(&download, stamp, None).unwrap() {
                Absorbed::AlreadyHeld(entry) => assert_eq!(entry.id, first.entry().id),
                other => panic!("identical bytes must not be stored again: {other:?}"),
            }
        }
        assert_eq!(vault.list().unwrap().len(), 1, "one world, one entry");

        // A world that differs in any way is a world worth keeping.
        fs::write(live.join("db").join("CURRENT"), b"MANIFEST-000002\n").unwrap();
        let moved_on = base.join("slot-later.mcworld");
        mcworld::pack(&live, &moved_on).unwrap();
        let added = vault.absorb_mcworld(&moved_on, "20260811-150000", None).unwrap();
        assert!(matches!(added, Absorbed::Added(_)), "no claim, so it stands alone");
        assert_eq!(vault.list().unwrap().len(), 2);

        // With a claim, the same arrival updates the slot's world instead.
        vault.set_realm_slot(first.entry().id.as_str(), 34391948, 2).unwrap();
        let updated = vault
            .absorb_mcworld(&moved_on, "20260811-160000", Some((34391948, 2)))
            .unwrap();
        assert!(matches!(updated, Absorbed::Updated(_)));
        assert_eq!(updated.entry().id, first.entry().id);
        assert_eq!(vault.list().unwrap().len(), 2, "updated, not added");
        assert_eq!(
            vault.snapshots_for_key(&first.entry().id).len(),
            1,
            "and what it replaced is kept"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// What went wrong in the wild, reproduced: an entry was the vault's copy
    /// of a Realm slot, the slot handed back a *different* world — its stored
    /// copy was a previous occupant, never re-backed up — and that world was
    /// written over the entry. One player's Maia World became Hardcore Mode.
    #[test]
    fn a_slot_serving_a_different_world_never_overwrites_the_entry() {
        let base = temp("wrongworld");
        let vault = Vault::open(base.join("vault")).unwrap();

        let mine = base.join("mine");
        make_world(&mine, "Maia World (10) - Copy");
        let mine_archive = base.join("mine.mcworld");
        mcworld::pack(&mine, &mine_archive).unwrap();
        let held = vault.absorb_mcworld(&mine_archive, "20260811-090000", None).unwrap();
        let held = held.entry().clone();
        vault.set_realm_slot(&held.id, 34391948, 2).unwrap();

        // The slot's stored copy is a world that used to be there.
        let stale = base.join("stale");
        make_world(&stale, "Hardcore Mode");
        fs::write(
            stale.join("level.dat"),
            level_dat::test_fixtures::synthetic_level_dat_with_seed(-1920971761),
        )
        .unwrap();
        let stale_archive = base.join("stale.mcworld");
        mcworld::pack(&stale, &stale_archive).unwrap();

        let absorbed = vault
            .absorb_mcworld(&stale_archive, "20260811-144832", Some((34391948, 2)))
            .unwrap();
        assert!(matches!(absorbed, Absorbed::Added(_)), "it is its own world: {absorbed:?}");
        assert_ne!(absorbed.entry().id, held.id);
        // And it does not inherit the slot: an old occupant is not the world
        // on it, so the copy that is stays the slot's.
        assert_eq!(vault.find_by_realm_slot(34391948, 2).unwrap().map(|e| e.id), Some(held.id.clone()));

        // The claimed entry is untouched — not replaced, and not even
        // snapshotted. Judged on the seed: both fixtures carry the same
        // `LevelName`, which is exactly the case a name could not tell apart.
        let after = vault.entry(&held.id).unwrap();
        assert_eq!(after.name, held.name);
        assert_eq!(world_seed(&after.world_dir), world_seed(&mine));
        assert_ne!(world_seed(&after.world_dir), world_seed(&stale));
        assert!(vault.snapshots_for_key(&held.id).is_empty());

        // The same world moving on still updates it, which is the point of the
        // claim in the first place.
        fs::write(mine.join("db").join("CURRENT"), b"MANIFEST-000002\n").unwrap();
        let moved_on = base.join("mine-later.mcworld");
        mcworld::pack(&mine, &moved_on).unwrap();
        let updated = vault
            .absorb_mcworld(&moved_on, "20260811-150000", Some((34391948, 2)))
            .unwrap();
        assert!(matches!(updated, Absorbed::Updated(_)), "{updated:?}");
        assert_eq!(updated.entry().id, held.id);

        let _ = fs::remove_dir_all(&base);
    }

    /// A file carries no provenance, so only identical content counts.
    #[test]
    fn an_identical_import_is_recognised_and_a_different_one_is_not() {
        let base = temp("sameimport");
        let live = base.join("w");
        make_world(&live, "Spike Test World");
        let vault = Vault::open(base.join("vault")).unwrap();

        let mcworld = base.join("shared.mcworld");
        mcworld::pack(&live, &mcworld).unwrap();
        let first = vault.import_mcworld(&mcworld, "20260811-100000").unwrap();
        let second = vault.import_mcworld(&mcworld, "20260811-110000").unwrap();

        let held = vault.find_same_content(&second.world_dir, &second.id).unwrap();
        assert_eq!(held.map(|e| e.id), Some(first.id.clone()));

        vault.forget(&second.id).unwrap();
        assert_eq!(vault.list().unwrap().len(), 1);
        assert!(!vault.library_dir().join(&second.id).exists());
        assert!(
            vault.snapshots_for_key(&second.id).is_empty(),
            "forgetting a copy leaves no backup: the world is still here"
        );

        // A world that only looks similar is left alone.
        fs::write(first.world_dir.join("db").join("CURRENT"), b"MANIFEST-000002\n").unwrap();
        let changed = vault.import_mcworld(&mcworld, "20260811-120000").unwrap();
        assert!(vault.find_same_content(&changed.world_dir, &changed.id).unwrap().is_none());

        // And a vault id is the only thing `forget` will touch.
        assert!(vault.forget("../library").is_err());
        assert!(vault.forget("").is_err());

        let _ = fs::remove_dir_all(&base);
    }

    /// The reported case: the same world copied in twice showed up as two
    /// sections, because every import mints a fresh vault id.
    #[test]
    fn copies_of_one_world_share_a_backup_section() {
        let base = temp("mergecopies");
        let live = base.join("w");
        make_world(&live, "Spike Test World");
        let vault = Vault::open(base.join("vault")).unwrap();

        let mcworld = base.join("shared.mcworld");
        mcworld::pack(&live, &mcworld).unwrap();
        let first = vault.import_mcworld(&mcworld, "20260811-100000").unwrap();
        vault.snapshot(&first.id, &first.world_dir, "20260811-100100").unwrap();
        let second = vault.import_mcworld(&mcworld, "20260811-120000").unwrap();
        vault.snapshot(&second.id, &second.world_dir, "20260811-120100").unwrap();
        assert_ne!(first.id, second.id, "each import is its own vault entry");

        let groups = vault.all_backups(&vault.list().unwrap());
        assert_eq!(groups.len(), 1, "one world, one section: {groups:#?}");
        assert_eq!(groups[0].snapshots.len(), 2, "both copies' history is there");
        assert_eq!(groups[0].key, second.id, "the newest copy names the section");

        // The seed is cached beside the snapshots so this costs no archive
        // reads next time.
        assert!(vault.backups_dir().join(&first.id).join("seed.txt").is_file());

        let _ = fs::remove_dir_all(&base);
    }

    /// Two different worlds can share a name, so histories must never be
    /// merged on name alone.
    #[test]
    fn same_named_worlds_keep_separate_histories() {
        let base = temp("samename");
        let vault = Vault::open(base.join("vault")).unwrap();
        for (folder, stamp) in [("aaa=", "20260810-100000"), ("bbb=", "20260810-110000")] {
            let live = base.join("minecraftWorlds").join(folder);
            make_world(&live, "My World");
            vault.protect(&live, folder, stamp).unwrap();
        }

        let groups = vault.all_backups(&vault.list().unwrap());
        // Both carry the fixture's LevelName, so this is the name-collision
        // case: identical names, but they must stay two separate histories.
        assert_eq!(groups.len(), 2, "two distinct worlds, two histories");
        assert_eq!(groups[0].name, groups[1].name, "the names really do collide");
        assert_ne!(groups[0].key, groups[1].key);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn backups_stay_named_after_the_world_is_deleted() {
        let base = temp("backupnames");
        let live = base.join("w");
        make_world(&live, "Spike Test World");
        let vault = Vault::open(base.join("vault")).unwrap();
        let entry = vault.protect(&live, "abc=", "20260810-1400").unwrap();
        vault.delete(&entry.id, "20260810-1500").unwrap();

        let groups = vault.all_backups(&vault.list().unwrap());
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0].name, "Spike Test World",
            "a deleted world's backups must still show its name"
        );
        assert!(groups[0].snapshots.len() >= 2);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn move_vault_relocates_everything_and_verifies() {
        let base = temp("movevault");
        let live = base.join("w");
        make_world(&live, "Spike Test World");
        let old_root = base.join("old");
        let vault = Vault::open(&old_root).unwrap();
        let entry = vault.protect(&live, "abc=", "20260810-1400").unwrap();

        let new_root = base.join("new");
        let moved = move_vault(&old_root, &new_root, |_, _| {}).unwrap();
        assert!(moved > 0);
        assert!(!old_root.exists(), "the old vault is removed after verifying");

        let moved_vault = Vault::open(&new_root).unwrap();
        let entries = moved_vault.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Spike Test World");
        assert_eq!(entries[0].id, entry.id);
        assert!(!moved_vault.snapshots(&entries[0]).is_empty(), "backups came too");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn move_vault_refuses_a_non_empty_destination() {
        let base = temp("movebusy");
        let live = base.join("w");
        make_world(&live, "Spike Test World");
        let old_root = base.join("old");
        let vault = Vault::open(&old_root).unwrap();
        vault.protect(&live, "abc=", "20260810-1400").unwrap();

        let busy = base.join("busy");
        fs::create_dir_all(&busy).unwrap();
        fs::write(busy.join("something.txt"), b"in the way").unwrap();

        assert!(move_vault(&old_root, &busy, |_, _| {}).is_err());
        assert!(old_root.join("library").is_dir(), "source untouched on refusal");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn archive_refuses_non_world() {
        let base = temp("nonworld");
        fs::create_dir_all(base.join("empty")).unwrap();
        let vault = Vault::open(base.join("vault")).unwrap();
        assert!(vault.archive(&base.join("empty"), "x", "20260810-1600").is_err());
        let _ = fs::remove_dir_all(&base);
    }
}
