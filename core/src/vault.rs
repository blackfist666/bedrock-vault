//! The vault itself: an on-disk library of worlds plus backup/archive/activate.
//!
//! Layout (§5.1 of DESIGN.md):
//! ```text
//! <root>\library\<id>\world\   raw world folder, ready to copy
//!                    \meta.json
//!        \backups\<id>\<timestamp>.mcworld
//!        \exports\
//! ```
//!
//! Every operation that removes data takes a `.mcworld` backup first, and every
//! copy is verified (file count + total size) before the source is touched.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::guard;
use crate::level_dat;
use crate::mcworld;
use crate::scan;

pub struct Vault {
    pub root: PathBuf,
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
}

impl Vault {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        for sub in ["library", "backups", "exports"] {
            fs::create_dir_all(root.join(sub))
                .with_context(|| format!("creating {}", root.join(sub).display()))?;
        }
        Ok(Self { root })
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

    /// Every world currently in the library, newest-played first.
    pub fn list(&self) -> Result<Vec<LibraryEntry>> {
        let dir = self.library_dir();
        let mut out = Vec::new();
        if !dir.is_dir() {
            return Ok(out);
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let world_dir = entry.path().join("world");
            if !world_dir.join("level.dat").is_file() {
                continue;
            }
            let meta = fs::read(world_dir.join("level.dat"))
                .ok()
                .and_then(|d| level_dat::parse(&d).ok());
            let fallback = fs::read_to_string(world_dir.join("levelname.txt"))
                .ok()
                .map(|s| s.trim().to_owned());
            out.push(LibraryEntry {
                id: entry.file_name().to_string_lossy().into_owned(),
                name: meta
                    .as_ref()
                    .and_then(|m| m.name.clone())
                    .or(fallback)
                    .unwrap_or_else(|| "<unnamed>".into()),
                size_bytes: scan::dir_size(&world_dir),
                last_played: meta.as_ref().and_then(|m| m.last_played),
                version: meta.as_ref().and_then(|m| m.version.clone()),
                game_mode: meta.as_ref().map(|m| m.game_mode_label()).unwrap_or("-"),
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

    /// Snapshot a world folder into `backups\<id>\<timestamp>.mcworld`.
    pub fn backup(&self, world_dir: &Path, id: &str, stamp: &str) -> Result<PathBuf> {
        let dir = self.backups_dir().join(id);
        fs::create_dir_all(&dir)?;
        let out = dir.join(format!("{stamp}.mcworld"));
        mcworld::pack(world_dir, &out)?;
        Ok(out)
    }

    /// Move a live world out of `minecraftWorlds` into the library.
    ///
    /// Copy → verify → backup → delete source, so a failure at any point leaves
    /// the live world untouched.
    pub fn archive(&self, live_world: &Path, stamp: &str) -> Result<LibraryEntry> {
        guard::ensure_closed()?;
        if !live_world.join("level.dat").is_file() {
            bail!("{} is not a world folder", live_world.display());
        }
        let id = new_id(stamp);
        let dest_parent = self.library_dir().join(&id);
        let dest = dest_parent.join("world");
        fs::create_dir_all(&dest_parent)?;

        copy_verified(live_world, &dest).inspect_err(|_| {
            let _ = fs::remove_dir_all(&dest_parent);
        })?;
        self.backup(&dest, &id, stamp)?;

        fs::remove_dir_all(live_world)
            .with_context(|| format!("removing source {}", live_world.display()))?;
        self.entry(&id)
    }

    /// Copy a library world into `minecraftWorlds` under a fresh folder id.
    ///
    /// The library stays the source of truth (copy, not move), and a fresh id
    /// avoids colliding with worlds created in-game since archiving.
    pub fn activate(&self, id: &str, worlds_dir: &Path, stamp: &str) -> Result<PathBuf> {
        guard::ensure_closed()?;
        let entry = self.entry(id)?;
        fs::create_dir_all(worlds_dir)?;
        let dest = worlds_dir.join(new_folder_id(stamp));
        if dest.exists() {
            bail!("{} already exists", dest.display());
        }
        copy_verified(&entry.world_dir, &dest).inspect_err(|_| {
            let _ = fs::remove_dir_all(&dest);
        })?;
        Ok(dest)
    }

    /// Import an unpacked `.mcworld` (or any world folder) into the library.
    pub fn import_folder(&self, world_dir: &Path, stamp: &str) -> Result<LibraryEntry> {
        if !world_dir.join("level.dat").is_file() {
            bail!("{} has no level.dat", world_dir.display());
        }
        let id = new_id(stamp);
        let dest = self.library_dir().join(&id).join("world");
        fs::create_dir_all(dest.parent().unwrap())?;
        copy_verified(world_dir, &dest)?;
        self.entry(&id)
    }

    /// Import a `.mcworld` archive into the library.
    pub fn import_mcworld(&self, mcworld: &Path, stamp: &str) -> Result<LibraryEntry> {
        let id = new_id(stamp);
        let dest = self.library_dir().join(&id).join("world");
        fs::create_dir_all(dest.parent().unwrap())?;
        mcworld::unpack(mcworld, &dest).inspect_err(|_| {
            let _ = fs::remove_dir_all(self.library_dir().join(&id));
        })?;
        self.entry(&id)
    }

    /// Remove a library world, backing it up first.
    pub fn delete(&self, id: &str, stamp: &str) -> Result<PathBuf> {
        let entry = self.entry(id)?;
        let backup = self.backup(&entry.world_dir, id, &format!("{stamp}-deleted"))?;
        fs::remove_dir_all(self.library_dir().join(id))?;
        Ok(backup)
    }
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

/// Vault ids are timestamp-based: sortable, readable, and collision-free in
/// practice because a second operation in the same second gets a suffix.
fn new_id(stamp: &str) -> String {
    format!("w{stamp}")
}

/// Bedrock folder ids are arbitrary; a fresh one avoids in-game collisions.
fn new_folder_id(stamp: &str) -> String {
    format!("BV{stamp}")
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
    fn archive_then_activate_round_trip() {
        let base = temp("vault");
        let live = base.join("minecraftWorlds").join("abc=");
        make_world(&live, "Spike Test World");
        let vault = Vault::open(base.join("vault")).unwrap();

        let entry = vault.archive(&live, "20260810-1200").unwrap();
        assert_eq!(entry.name, "Spike Test World");
        assert!(!live.exists(), "source world should be gone after archive");
        assert!(vault.backups_dir().join(&entry.id).is_dir(), "backup taken");
        assert_eq!(vault.list().unwrap().len(), 1);

        let worlds_dir = base.join("minecraftWorlds");
        let activated = vault.activate(&entry.id, &worlds_dir, "20260810-1300").unwrap();
        assert!(activated.join("level.dat").is_file());
        assert_eq!(
            fs::read(activated.join("db").join("CURRENT")).unwrap(),
            b"MANIFEST-000001\n"
        );
        // Library keeps its copy: it is the source of truth.
        assert_eq!(vault.list().unwrap().len(), 1);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn import_mcworld_and_delete_keeps_backup() {
        let base = temp("import");
        let src = base.join("src");
        make_world(&src, "Spike Test World");
        let archive = base.join("w.mcworld");
        mcworld::pack(&src, &archive).unwrap();

        let vault = Vault::open(base.join("vault")).unwrap();
        let entry = vault.import_mcworld(&archive, "20260810-1400").unwrap();
        assert_eq!(entry.name, "Spike Test World");

        let backup = vault.delete(&entry.id, "20260810-1500").unwrap();
        assert!(backup.is_file(), "delete must leave a backup behind");
        assert!(vault.list().unwrap().is_empty());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn archive_refuses_non_world() {
        let base = temp("nonworld");
        fs::create_dir_all(base.join("empty")).unwrap();
        let vault = Vault::open(base.join("vault")).unwrap();
        assert!(vault.archive(&base.join("empty"), "20260810-1600").is_err());
        let _ = fs::remove_dir_all(&base);
    }
}
