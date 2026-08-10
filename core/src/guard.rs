//! Process guard: never touch world data while Minecraft holds it open.
//!
//! Copying a world's `db/` LevelDB while the game has it open produces a
//! corrupt copy, so every destructive vault operation checks this first.

use std::process::Command;

/// Executable names used by the Bedrock client across install types.
pub const MINECRAFT_PROCESSES: &[&str] = &["Minecraft.Windows.exe", "Minecraft.exe"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameStatus {
    /// Process names currently running (empty = safe to operate).
    pub running: Vec<String>,
}

impl GameStatus {
    pub fn is_running(&self) -> bool {
        !self.running.is_empty()
    }
}

/// Ask Windows which Minecraft processes are running.
///
/// A failure to query is reported as "not running" rather than an error: the
/// caller still verifies its work, and a broken tasklist should not make the
/// whole app unusable. Callers that need certainty check [`GameStatus`].
pub fn game_status() -> GameStatus {
    let mut running = Vec::new();
    for name in MINECRAFT_PROCESSES {
        if process_exists(name) {
            running.push((*name).to_owned());
        }
    }
    GameStatus { running }
}

#[cfg(windows)]
fn process_exists(exe: &str) -> bool {
    // tasklist prints a "no tasks" banner (not an error) when nothing matches.
    let Ok(out) = Command::new("tasklist")
        .args(["/FI", &format!("IMAGENAME eq {exe}"), "/NH", "/FO", "CSV"])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&out.stdout).contains(exe)
}

#[cfg(not(windows))]
fn process_exists(_exe: &str) -> bool {
    let _ = Command::new("true");
    false
}

/// Error returned by vault operations refused because the game is open.
pub fn ensure_closed() -> anyhow::Result<()> {
    let status = game_status();
    if status.is_running() {
        anyhow::bail!(
            "Minecraft is running ({}) — close the game before moving world data",
            status.running.join(", ")
        );
    }
    Ok(())
}
