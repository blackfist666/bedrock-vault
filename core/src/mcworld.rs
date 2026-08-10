//! Pack a world folder into a `.mcworld` (zip with files at the root) and
//! unpack one back into a folder, validating `level.dat` on the way out.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use zip::write::SimpleFileOptions;

pub fn pack(world_dir: &Path, out: &Path) -> Result<u64> {
    if !world_dir.join("level.dat").is_file() {
        bail!("{} has no level.dat — not a world folder", world_dir.display());
    }
    let file = File::create(out)
        .with_context(|| format!("creating {}", out.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);

    let mut count = 0u64;
    for entry in walkdir::WalkDir::new(world_dir).sort_by_file_name() {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        // .mcworld requires paths relative to the world folder, at the zip root,
        // with forward slashes.
        let rel = entry
            .path()
            .strip_prefix(world_dir)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        zip.start_file(&rel, options)?;
        let mut src = File::open(entry.path())
            .with_context(|| format!("reading {}", entry.path().display()))?;
        io::copy(&mut src, &mut zip)?;
        count += 1;
    }
    zip.finish()?.flush()?;
    Ok(count)
}

pub fn unpack(mcworld: &Path, dest: &Path) -> Result<u64> {
    if dest.exists() && fs::read_dir(dest)?.next().is_some() {
        bail!("{} already exists and is not empty", dest.display());
    }
    let file = File::open(mcworld)
        .with_context(|| format!("opening {}", mcworld.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("reading zip")?;

    let mut count = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let Some(rel) = entry.enclosed_name() else {
            bail!("zip entry {} has an unsafe path", entry.name());
        };
        let target = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = File::create(&target)
            .with_context(|| format!("creating {}", target.display()))?;
        io::copy(&mut entry, &mut out)?;
        count += 1;
    }

    if !dest.join("level.dat").is_file() {
        bail!("unpacked archive has no level.dat at the root — not a valid .mcworld");
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level_dat::{self, test_fixtures};

    #[test]
    fn pack_unpack_round_trip() {
        let base = std::env::temp_dir().join(format!("vault-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let world = base.join("world");
        fs::create_dir_all(world.join("db")).unwrap();
        fs::write(world.join("level.dat"), test_fixtures::synthetic_level_dat()).unwrap();
        fs::write(world.join("levelname.txt"), "Spike Test World").unwrap();
        fs::write(world.join("db").join("CURRENT"), b"MANIFEST-000001\n").unwrap();

        let mcworld = base.join("out.mcworld");
        assert_eq!(pack(&world, &mcworld).unwrap(), 3);

        let restored = base.join("restored");
        assert_eq!(unpack(&mcworld, &restored).unwrap(), 3);

        for rel in ["level.dat", "levelname.txt", "db/CURRENT"] {
            assert_eq!(
                fs::read(world.join(rel)).unwrap(),
                fs::read(restored.join(rel)).unwrap(),
                "content mismatch in {rel}"
            );
        }
        let meta = level_dat::parse(&fs::read(restored.join("level.dat")).unwrap()).unwrap();
        assert_eq!(meta.name.as_deref(), Some("Spike Test World"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn pack_refuses_non_world_folder() {
        let base = std::env::temp_dir().join(format!("vault-test-nw-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        assert!(pack(&base, &base.join("x.mcworld")).is_err());
        let _ = fs::remove_dir_all(&base);
    }
}
