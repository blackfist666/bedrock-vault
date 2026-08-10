//! Installed marketplace/store content and per-world pack usage.
//!
//! GDK keeps downloaded store content under
//! `%APPDATA%\Minecraft Bedrock\premium_cache\<category>\<id>\` with a readable
//! `manifest.json`; UWP keeps the same layout under `LocalState\premium_cache`.
//! Pack display names are usually the localization key `pack.name`, resolved
//! from `texts\<locale>.lang` inside the pack.
//!
//! Worlds reference the packs they use by uuid + version in
//! `world_resource_packs.json` / `world_behavior_packs.json`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub const CATEGORIES: &[(&str, &str)] = &[
    ("world_templates", "World templates"),
    ("resource_packs", "Resource packs"),
    ("behavior_packs", "Behavior packs"),
    ("skin_packs", "Skin packs"),
];

#[derive(Debug)]
pub struct Pack {
    pub category: &'static str,
    pub uuid: String,
    pub name: String,
    pub version: String,
}

#[derive(Debug)]
pub struct PackRef {
    pub uuid: String,
    pub version: String,
}

/// Every premium_cache directory on this machine, labelled by install type.
pub fn find_premium_caches() -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    if let Ok(roaming) = std::env::var("APPDATA") {
        let p = Path::new(&roaming).join(r"Minecraft Bedrock\premium_cache");
        if p.is_dir() {
            found.push(("GDK".to_owned(), p));
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        for pkg in crate::scan::PACKAGE_IDS {
            let p = Path::new(&local)
                .join("Packages")
                .join(pkg)
                .join(r"LocalState\premium_cache");
            if p.is_dir() {
                let label = if pkg.contains("Beta") { "UWP preview" } else { "UWP" };
                found.push((label.to_owned(), p));
            }
        }
    }
    found
}

/// Scan one premium_cache directory (all categories + persona count).
pub fn scan_premium_cache(cache: &Path) -> Result<(Vec<Pack>, usize)> {
    let mut packs = Vec::new();
    for (dir_name, _) in CATEGORIES {
        let dir = cache.join(dir_name);
        if !dir.is_dir() {
            continue;
        }
        let mut entries: Vec<_> = fs::read_dir(&dir)?.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if !entry.path().is_dir() {
                continue;
            }
            match read_pack(&entry.path(), dir_name) {
                Ok(p) => {
                    // Templates bundle their own packs (resource_packs/rp0, …);
                    // worlds created from the template reference those uuids,
                    // so they must be in the index too.
                    if *dir_name == "world_templates" {
                        for sub in ["resource_packs", "behavior_packs"] {
                            packs.extend(embedded_packs(&entry.path().join(sub), &p.name));
                        }
                    }
                    packs.push(p);
                }
                Err(e) => eprintln!(
                    "warning: skipping {}\\{}: {e:#}",
                    dir_name,
                    entry.file_name().to_string_lossy()
                ),
            }
        }
    }
    let persona_count = fs::read_dir(cache.join("persona"))
        .map(|it| it.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).count())
        .unwrap_or(0);
    Ok((packs, persona_count))
}

fn read_pack(dir: &Path, category: &'static str) -> Result<Pack> {
    let raw = fs::read_to_string(dir.join("manifest.json")).context("reading manifest.json")?;
    let manifest: serde_json::Value =
        serde_json::from_str(raw.trim_start_matches('\u{feff}')).context("parsing manifest.json")?;
    let header = &manifest["header"];
    let uuid = header["uuid"].as_str().unwrap_or("?").to_owned();
    let raw_name = header["name"].as_str().unwrap_or("").to_owned();
    let name = resolve_name(dir, &raw_name);
    let version = version_string(&header["version"]);
    Ok(Pack { category, uuid, name, version })
}

/// Packs bundled inside a world template; unresolvable names fall back to the
/// template's own name so the world-usage join always shows something useful.
fn embedded_packs(dir: &Path, template_name: &str) -> Vec<Pack> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| read_pack(&e.path(), "template_embedded").ok())
        .map(|mut p| {
            if looks_like_loc_key(&p.name) {
                p.name = template_name.to_owned();
            }
            p
        })
        .collect()
}

/// True for unresolved localization keys like `pack.name` or `<unnamed>`.
fn looks_like_loc_key(name: &str) -> bool {
    name == "<unnamed>" || (name.starts_with("pack.") && !name.contains(' '))
}

/// Manifest names are usually the literal key `pack.name`; the real name lives
/// in the pack's .lang files. Skin packs title themselves via `skinpack.<Id>=`
/// instead, and some packs use a custom key (e.g. `pack.CherryBlossomVilla`).
fn resolve_name(pack_dir: &Path, raw: &str) -> String {
    if !raw.is_empty() && !looks_like_loc_key(raw) && raw != "pack.name" {
        return raw.to_owned();
    }
    let texts = pack_dir.join("texts");
    let mut lang_files: Vec<PathBuf> =
        ["en_US.lang", "en_GB.lang"].iter().map(|l| texts.join(l)).collect();
    if let Ok(entries) = fs::read_dir(&texts) {
        for entry in entries.filter_map(|e| e.ok()) {
            if entry.path().extension().is_some_and(|x| x == "lang") {
                lang_files.push(entry.path());
            }
        }
    }
    for path in lang_files {
        if let Some(name) = lang_lookup(&path, raw) {
            return name;
        }
    }
    if raw.is_empty() { "<unnamed>".into() } else { raw.to_owned() }
}

/// Find a display name in one .lang file: the manifest's own key first, then
/// `pack.name`, then the first `skinpack.*` title.
fn lang_lookup(path: &Path, manifest_key: &str) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    let mut pack_name = None;
    let mut skinpack = None;
    for line in content.lines() {
        let line = line.trim_start_matches('\u{feff}');
        let Some((k, v)) = line.split_once('=') else { continue };
        let k = k.trim();
        // Trailing "\t## comment" is legal in .lang values.
        let v = v.split("\t#").next().unwrap_or(v).trim();
        if v.is_empty() {
            continue;
        }
        if !manifest_key.is_empty() && k == manifest_key {
            return Some(v.to_owned());
        }
        if k == "pack.name" && pack_name.is_none() {
            pack_name = Some(v.to_owned());
        }
        if k.starts_with("skinpack.") && skinpack.is_none() {
            skinpack = Some(v.to_owned());
        }
    }
    pack_name.or(skinpack)
}

fn version_string(v: &serde_json::Value) -> String {
    match v.as_array() {
        Some(parts) => parts
            .iter()
            .filter_map(|p| p.as_i64())
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("."),
        // Older worlds store the version as the literal string "[1,0,55]".
        None => v
            .as_str()
            .unwrap_or("?")
            .trim_matches(['[', ']'])
            .replace(',', ".")
            .replace(' ', ""),
    }
}

/// Read a world's pack references: (resource, behavior).
pub fn world_pack_refs(world_dir: &Path) -> (Vec<PackRef>, Vec<PackRef>) {
    (
        read_refs(&world_dir.join("world_resource_packs.json")),
        read_refs(&world_dir.join("world_behavior_packs.json")),
    )
}

fn read_refs(path: &Path) -> Vec<PackRef> {
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(raw.trim_start_matches('\u{feff}'))
    else {
        return Vec::new();
    };
    json.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(PackRef {
                        uuid: item["pack_id"].as_str()?.to_owned(),
                        version: version_string(&item["version"]),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(root: &Path) {
        let pack = root.join("resource_packs").join("abc=");
        fs::create_dir_all(pack.join("texts")).unwrap();
        fs::write(
            pack.join("manifest.json"),
            r#"{"format_version":2,"header":{"name":"pack.name","uuid":"94efe213-5492-b5bb-a48b-7d651918f2ab","version":[1,0,12]}}"#,
        )
        .unwrap();
        fs::write(
            pack.join("texts").join("en_US.lang"),
            "## comment\npack.name=Dragons Test Pack\t## trailing\npack.description=x\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("persona").join("p1")).unwrap();
    }

    #[test]
    fn reads_premium_cache_with_lang_names() {
        let root = std::env::temp_dir().join(format!("vault-packs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fixture(&root);

        let (packs, persona) = scan_premium_cache(&root).unwrap();
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].name, "Dragons Test Pack");
        assert_eq!(packs[0].uuid, "94efe213-5492-b5bb-a48b-7d651918f2ab");
        assert_eq!(packs[0].version, "1.0.12");
        assert_eq!(packs[0].category, "resource_packs");
        assert_eq!(persona, 1);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reads_world_pack_refs() {
        let root = std::env::temp_dir().join(format!("vault-refs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("world_resource_packs.json"),
            "[\n\t{\n\t\t\"pack_id\" : \"94efe213-5492-b5bb-a48b-7d651918f2ab\",\n\t\t\"version\" : [ 1, 0, 12 ]\n\t}\n]",
        )
        .unwrap();

        let (rp, bp) = world_pack_refs(&root);
        assert_eq!(rp.len(), 1);
        assert_eq!(rp[0].uuid, "94efe213-5492-b5bb-a48b-7d651918f2ab");
        assert_eq!(rp[0].version, "1.0.12");
        assert!(bp.is_empty());

        let _ = fs::remove_dir_all(&root);
    }
}
