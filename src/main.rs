mod level_dat;
mod mcworld;
mod nbt;
mod packs;
mod scan;

use std::path::PathBuf;

use anyhow::Result;
use chrono::{Local, TimeZone};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "vault", version, about = "Bedrock Vault M0 spike")]
struct Cli {
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
    /// Pack a world folder into a .mcworld
    Pack { world_dir: PathBuf, out: PathBuf },
    /// Unpack a .mcworld into a new folder and validate it
    Unpack { mcworld: PathBuf, dest: PathBuf },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Scan { dir } => cmd_scan(dir),
        Cmd::Packs => cmd_packs(),
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
