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

/// Provenance and sync state, stored beside each library world.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Meta {
    /// `minecraftWorlds` folder id this entry mirrors, if it is in the game.
    origin_folder: Option<String>,
    /// Where the world came from: 'local' | 'imported' | 'restored'.
    #[serde(default)]
    origin: String,
    /// `LastPlayed` of the source when the copy was taken; drives staleness.
    source_last_played: Option<i64>,
    /// When the vault copy was last written.
    synced_at: Option<i64>,
    /// Size of the copy, recorded when it was written.
    ///
    /// Cached because listing the library otherwise walks every file of every
    /// world, which makes a refresh after a bulk save look like a hang.
    size_bytes: Option<u64>,
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
    pub synced_at: Option<i64>,
    /// `LastPlayed` of the source when this copy was taken.
    pub source_last_played: Option<i64>,
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
                synced_at: meta.synced_at,
                source_last_played: meta.source_last_played,
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
        if meta.origin.is_empty() {
            meta.origin = "local".into();
        }
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
        meta.origin_folder = Some(folder_id);
        meta.source_last_played = entry.last_played;
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
            origin: origin.to_owned(),
            source_last_played: entry.last_played,
            synced_at: Some(now()),
            size_bytes: Some(scan::dir_size(&entry.world_dir)),
        };
        self.write_meta(id, &meta)?;
        self.entry(id)
    }

    /// Remove a world from the library, leaving a final snapshot behind.
    pub fn delete(&self, id: &str, stamp: &str) -> Result<PathBuf> {
        let entry = self.entry(id)?;
        let backup = self.snapshot_named(id, &entry.world_dir, &format!("{stamp}-deleted"))?;
        fs::remove_dir_all(self.library_dir().join(id))?;
        Ok(backup)
    }

    /// Take a snapshot and prune old ones per the retention setting.
    pub fn snapshot(&self, id: &str, world_dir: &Path, stamp: &str) -> Result<PathBuf> {
        let path = self.snapshot_named(id, world_dir, stamp)?;
        self.prune_snapshots(id)?;
        Ok(path)
    }

    fn snapshot_named(&self, id: &str, world_dir: &Path, stamp: &str) -> Result<PathBuf> {
        let dir = self.backups_dir().join(id);
        fs::create_dir_all(&dir)?;
        let out = dir.join(format!("{stamp}.mcworld"));
        mcworld::pack(world_dir, &out)?;
        // Remember the world's name so backups stay identifiable after the
        // world itself is deleted from the vault.
        if let Some(name) = world_name(world_dir) {
            let _ = fs::write(dir.join("name.txt"), name);
        }
        Ok(out)
    }

    /// All snapshots on disk, grouped per world, newest group first.
    ///
    /// `library` supplies current names; pass the list you already have.
    pub fn all_backups(&self, library: &[LibraryEntry]) -> Vec<BackupGroup> {
        let Ok(entries) = fs::read_dir(self.backups_dir()) else {
            return Vec::new();
        };
        let live_names: std::collections::HashMap<&str, &str> = library
            .iter()
            .map(|e| (e.id.as_str(), e.name.as_str()))
            .collect();

        let mut groups: Vec<BackupGroup> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| {
                let key = e.file_name().to_string_lossy().into_owned();
                let snapshots = self.snapshots_for_key(&key);
                if snapshots.is_empty() {
                    return None;
                }
                let name = live_names
                    .get(key.as_str())
                    .map(|s| (*s).to_owned())
                    .or_else(|| {
                        fs::read_to_string(e.path().join("name.txt"))
                            .ok()
                            .map(|s| s.trim().to_owned())
                            .filter(|s| !s.is_empty())
                    })
                    .unwrap_or_else(|| key.clone());
                Some(BackupGroup { key, name, snapshots })
            })
            .collect();
        groups.sort_by(|a, b| {
            let a_newest = a.snapshots.first().map(|s| s.stamp.clone()).unwrap_or_default();
            let b_newest = b.snapshots.first().map(|s| s.stamp.clone()).unwrap_or_default();
            b_newest.cmp(&a_newest)
        });
        groups
    }

    /// Snapshot history for an entry, newest first.
    ///
    /// Also picks up snapshots filed under the world's `minecraftWorlds` folder
    /// id, so history taken before the world was protected is not orphaned.
    pub fn snapshots(&self, entry: &LibraryEntry) -> Vec<Snapshot> {
        let mut keys = vec![entry.id.clone()];
        if let Some(folder) = &entry.origin_folder {
            keys.push(folder.clone());
        }
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
        assert!(
            vault.snapshots(&second).len() >= 2,
            "history keeps the pre-sync snapshot"
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

    #[test]
    fn retention_prunes_old_snapshots_but_keeps_deletions() {
        let base = temp("retention");
        let live = base.join("w");
        make_world(&live, "Spike Test World");
        let mut vault = Vault::open(base.join("vault")).unwrap();
        vault.settings.snapshot_retention = 2;

        let entry = vault.protect(&live, "abc=", "20260810-100000").unwrap();
        for stamp in ["20260810-110000", "20260810-120000", "20260810-130000"] {
            vault.snapshot(&entry.id, &entry.world_dir, stamp).unwrap();
        }
        assert_eq!(vault.snapshots_for_key(&entry.id).len(), 2);

        vault.snapshot_named(&entry.id, &entry.world_dir, "20260810-140000-deleted").unwrap();
        vault.snapshot(&entry.id, &entry.world_dir, "20260810-150000").unwrap();
        let stamps: Vec<_> = vault.snapshots_for_key(&entry.id).into_iter().map(|s| s.stamp).collect();
        assert!(
            stamps.iter().any(|s| s.ends_with("-deleted")),
            "deletion snapshots survive pruning: {stamps:?}"
        );

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
    fn archive_refuses_non_world() {
        let base = temp("nonworld");
        fs::create_dir_all(base.join("empty")).unwrap();
        let vault = Vault::open(base.join("vault")).unwrap();
        assert!(vault.archive(&base.join("empty"), "x", "20260810-1600").is_err());
        let _ = fs::remove_dir_all(&base);
    }
}
