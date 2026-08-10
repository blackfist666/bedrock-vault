//! Process guard: never touch world data while Minecraft holds it open.
//!
//! Copying a world's `db/` LevelDB while the game has it open produces a
//! corrupt copy, so every destructive vault operation checks this first.
//!
//! The check asks Windows for the process list directly. It must never shell
//! out to `tasklist`: spawning a console program from a GUI app flashes a
//! console window and steals focus, which is intolerable for something the UI
//! polls in the background.

/// Any running process whose image name contains this is treated as the game.
///
/// Matching on a substring rather than an exact list is deliberate: Bedrock
/// ships under several names across install types (`Minecraft.Windows.exe` for
/// the Store/UWP build, `Minecraft.exe` for the Xbox launcher build) and a name
/// this guard has not heard of must fail *safe*, not silently allow a copy.
const GAME_MARKER: &str = "minecraft";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
pub fn game_status() -> GameStatus {
    let mut running: Vec<String> = running_processes()
        .into_iter()
        .filter(|name| name.to_lowercase().contains(GAME_MARKER))
        .collect();
    running.sort();
    running.dedup();
    GameStatus { running }
}

/// Every running process image name, via the Toolhelp snapshot API.
///
/// No child process, no window, ~1ms.
#[cfg(windows)]
fn running_processes() -> Vec<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let mut names = Vec::new();
    // SAFETY: the snapshot handle is checked before use and closed on every
    // path; PROCESSENTRY32W is zeroed and given its required dwSize.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return names;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let end = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                names.push(String::from_utf16_lossy(&entry.szExeFile[..end]));
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }
    names
}

#[cfg(not(windows))]
fn running_processes() -> Vec<String> {
    Vec::new()
}

/// Refuse an operation while the game is open.
///
/// Every destructive operation calls this, so safety does not depend on how
/// often the UI polls — the poll only drives the on-screen warning.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard is only as good as its ability to see processes at all, and it
    /// fails *open* (reports "not running") when the query breaks — so prove
    /// the query works by finding the test binary itself.
    #[test]
    #[cfg(windows)]
    fn process_listing_finds_this_process() {
        let me = std::env::current_exe().unwrap();
        let my_name = me.file_name().unwrap().to_string_lossy().to_lowercase();
        let processes = running_processes();
        assert!(
            !processes.is_empty(),
            "process snapshot returned nothing — the process guard cannot work"
        );
        assert!(
            processes.iter().any(|p| p.to_lowercase() == my_name),
            "expected to find '{my_name}' among {} running processes",
            processes.len()
        );
    }

    #[test]
    #[cfg(windows)]
    fn matching_is_case_insensitive_and_substring_based() {
        for name in ["Minecraft.Windows.exe", "Minecraft.exe", "minecraft.exe"] {
            assert!(
                name.to_lowercase().contains(GAME_MARKER),
                "'{name}' must be recognised as the game"
            );
        }
        assert!(!"bedrock-vault.exe".to_lowercase().contains(GAME_MARKER));
    }
}
