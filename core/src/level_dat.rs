//! Extract the handful of `level.dat` fields Bedrock Vault cares about.

use anyhow::{Context, Result};

use crate::nbt;

#[derive(Debug, Clone)]
pub struct WorldMeta {
    #[allow(dead_code)] // needed for write-back (rename) in Tier 1
    pub storage_version: i32,
    pub name: Option<String>,
    pub seed: Option<i64>,
    pub game_type: Option<i64>,
    #[allow(dead_code)] // surfaced in the Tier 1 UI, not in the spike table
    pub difficulty: Option<i64>,
    pub last_played: Option<i64>,
    pub version: Option<String>,
}

impl WorldMeta {
    pub fn game_mode_label(&self) -> &'static str {
        match self.game_type {
            Some(0) => "Survival",
            Some(1) => "Creative",
            Some(2) => "Adventure",
            Some(_) => "?",
            None => "-",
        }
    }
}

pub fn parse(data: &[u8]) -> Result<WorldMeta> {
    let level = nbt::parse_level_dat(data)?;
    let root = level.root.as_compound().context("root is not a compound")?;

    let version = root.get("lastOpenedWithVersion").and_then(|v| v.as_list()).map(|items| {
        items
            .iter()
            .filter_map(|i| i.as_i64())
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(".")
    });

    Ok(WorldMeta {
        storage_version: level.storage_version,
        name: root.get("LevelName").and_then(|v| v.as_str()).map(str::to_owned),
        seed: root.get("RandomSeed").and_then(|v| v.as_i64()),
        game_type: root.get("GameType").and_then(|v| v.as_i64()),
        difficulty: root.get("Difficulty").and_then(|v| v.as_i64()),
        last_played: root.get("LastPlayed").and_then(|v| v.as_i64()),
        version,
    })
}

#[cfg(test)]
pub mod test_fixtures {
    /// The same world after another session: identical in every way except
    /// `LastPlayed`, and therefore identical in length. What a world that has
    /// been played looks like to anything comparing sizes.
    pub fn synthetic_level_dat_with_last_played(when: i64) -> Vec<u8> {
        let mut out = synthetic_level_dat();
        let at = out
            .windows(8)
            .position(|w| w == 1754000000i64.to_le_bytes())
            .expect("the fixture carries a LastPlayed");
        out[at..at + 8].copy_from_slice(&when.to_le_bytes());
        out
    }

    /// Build a minimal but structurally faithful Bedrock `level.dat`:
    /// 8-byte header + LE NBT compound with the fields the app reads.
    pub fn synthetic_level_dat() -> Vec<u8> {
        fn name(s: &str) -> Vec<u8> {
            let mut v = (s.len() as u16).to_le_bytes().to_vec();
            v.extend_from_slice(s.as_bytes());
            v
        }

        let mut p: Vec<u8> = Vec::new();
        p.push(10); // root compound
        p.extend(name(""));

        p.push(8); // TAG_String LevelName
        p.extend(name("LevelName"));
        p.extend(name("Spike Test World"));

        p.push(4); // TAG_Long RandomSeed
        p.extend(name("RandomSeed"));
        p.extend((-4406166776699799973i64).to_le_bytes());

        p.push(3); // TAG_Int GameType
        p.extend(name("GameType"));
        p.extend(1i32.to_le_bytes());

        p.push(3); // TAG_Int Difficulty
        p.extend(name("Difficulty"));
        p.extend(2i32.to_le_bytes());

        p.push(4); // TAG_Long LastPlayed
        p.extend(name("LastPlayed"));
        p.extend(1754000000i64.to_le_bytes());

        p.push(9); // TAG_List<Int> lastOpenedWithVersion
        p.extend(name("lastOpenedWithVersion"));
        p.push(3);
        p.extend(5i32.to_le_bytes());
        for n in [1i32, 21, 62, 1, 0] {
            p.extend(n.to_le_bytes());
        }

        p.push(1); // TAG_Byte (unread field, exercises skipping)
        p.extend(name("commandsEnabled"));
        p.push(1);

        p.push(0); // end of root compound

        let mut out = 10i32.to_le_bytes().to_vec(); // storage_version
        out.extend((p.len() as i32).to_le_bytes());
        out.extend(p);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synthetic_level_dat() {
        let meta = parse(&test_fixtures::synthetic_level_dat()).unwrap();
        assert_eq!(meta.storage_version, 10);
        assert_eq!(meta.name.as_deref(), Some("Spike Test World"));
        assert_eq!(meta.seed, Some(-4406166776699799973));
        assert_eq!(meta.game_type, Some(1));
        assert_eq!(meta.game_mode_label(), "Creative");
        assert_eq!(meta.difficulty, Some(2));
        assert_eq!(meta.last_played, Some(1754000000));
        assert_eq!(meta.version.as_deref(), Some("1.21.62.1.0"));
    }

    #[test]
    fn rejects_truncated_payload() {
        let data = test_fixtures::synthetic_level_dat();
        assert!(parse(&data[..data.len() - 10]).is_err());
    }
}
