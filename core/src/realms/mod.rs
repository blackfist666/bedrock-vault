//! Bedrock Realms client.
//!
//! Talks to `pocket.realms.minecraft.net`, an **unofficial, community-mapped
//! API**. It can change without notice, so every failure surfaces the service's
//! own message rather than a guess, and nothing here is required for the rest
//! of the app to work: Realms is strictly additive to the local vault.
//!
//! Implemented in Rust rather than via the `prismarine-*` Node libraries the
//! design doc originally proposed — bundling a Node runtime would add ~50 MB to
//! a 2.4 MB installer, and the whole protocol is HTTPS and JSON.

pub mod auth;
pub mod cache;
pub mod profile;

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

use crate::scan;


const REALMS_BASE: &str = "https://pocket.realms.minecraft.net";
/// Sent when the installed game's version cannot be determined. Realms rejects
/// clients it considers outdated, so this is a floor, not a preference.
const FALLBACK_CLIENT_VERSION: &str = "1.21.0";

/// One Realm the signed-in account can see.
#[derive(Debug, Clone, Serialize)]
pub struct Realm {
    pub id: i64,
    pub name: String,
    pub motd: String,
    /// `OPEN`, `CLOSED`, or whatever the service reports.
    pub state: String,
    /// Whether the signed-in account owns this Realm.
    ///
    /// `None` when it cannot be determined, which is deliberately distinct from
    /// "not yours": guessing wrong here would mislead about what can be
    /// uploaded, since only an owner may replace a Realm's world.
    pub owner: Option<bool>,
    /// XUID of the Realm's owner.
    pub owner_uuid: String,
    pub expired: bool,
    pub days_left: Option<i64>,
    /// Which of the Realm's slots is live, when reported.
    pub active_slot: Option<i64>,
    pub player_count: Option<i64>,
    pub max_players: Option<i64>,
}

impl Realm {
    /// What this account may do here, in words.
    pub fn role(&self) -> &'static str {
        match self.owner {
            Some(true) => "yours",
            Some(false) => "joined",
            None => "?",
        }
    }

    /// How long is left, in words.
    pub fn subscription(&self) -> String {
        match (self.expired, self.days_left) {
            (true, _) => "expired".to_owned(),
            (false, Some(d)) if d <= 0 => "expired".to_owned(),
            (false, Some(1)) => "1 day left".to_owned(),
            (false, Some(d)) => format!("{d} days left"),
            (false, None) => "active".to_owned(),
        }
    }
}

/// The Bedrock version to claim, taken from the newest world on this machine.
///
/// Realms refuses clients it thinks are out of date, and the installed game is
/// the most reliable statement of what version this PC is really on.
pub fn client_version() -> String {
    // Escape hatch: Realms can refuse a client it considers behind the Realm's
    // own server version, and the installed game is not always the answer.
    if let Ok(forced) = std::env::var("BEDROCK_VAULT_CLIENT_VERSION") {
        if !forced.trim().is_empty() {
            return forced.trim().to_owned();
        }
    }

    let newest = scan::find_worlds_dirs()
        .ok()
        .into_iter()
        .flatten()
        .filter(|loc| loc.path.is_dir())
        .filter_map(|loc| scan::scan(&loc.path).ok())
        .flatten()
        .filter_map(|w| w.meta.ok().and_then(|m| m.version))
        .max_by_key(|v| version_key(v));

    newest
        .map(|v| {
            // lastOpenedWithVersion has five parts; Realms wants three.
            v.split('.').take(3).collect::<Vec<_>>().join(".")
        })
        .unwrap_or_else(|| FALLBACK_CLIENT_VERSION.to_owned())
}

fn version_key(v: &str) -> Vec<i64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// The signed-in session, refreshing the token when it has aged out.
///
/// A token saved before the XUID was captured counts as stale too: without it,
/// an owned Realm cannot be told from a joined one.
pub fn session(force_refresh: bool) -> Result<auth::XstsToken> {
    let mut saved = cache::load();
    let microsoft = saved
        .microsoft
        .clone()
        .context("not signed in")?;

    if let Some(token) = &saved.realms {
        if !force_refresh && !token.is_expired() && token.xuid.is_some() {
            return Ok(token.clone());
        }
    }

    let refreshed = auth::refresh(&microsoft.refresh_token)?;
    let (token, identity) = auth::realms_session(&refreshed.access_token)?;
    saved.microsoft = Some(refreshed);
    saved.realms = Some(token.clone());
    saved.identity = identity;
    // The picture and gamertag can change; re-read them with the tokens.
    saved.profile = saved.identity.as_ref().and_then(|id| profile::fetch(id).ok());
    cache::save(&saved)?;
    Ok(token)
}

/// The signed-in player's profile, fetching it if it is not cached yet.
pub fn who_am_i() -> Option<profile::Profile> {
    let mut saved = cache::load();
    if saved.profile.is_some() {
        return saved.profile;
    }
    let identity = saved.identity.clone()?;
    let fetched = profile::fetch(&identity).ok()?;
    saved.profile = Some(fetched.clone());
    let _ = cache::save(&saved);
    Some(fetched)
}

/// Finish a device-code sign-in and save the session.
pub fn complete_login(tokens: auth::MicrosoftTokens) -> Result<auth::XstsToken> {
    let (token, identity) = auth::realms_session(&tokens.access_token)?;
    let profile = identity.as_ref().and_then(|id| profile::fetch(id).ok());
    cache::save(&cache::Session {
        microsoft: Some(tokens),
        realms: Some(token.clone()),
        identity,
        profile,
    })?;
    Ok(token)
}

/// The service's raw `/worlds` response, for inspecting an API that can change.
pub fn list_raw(session: &auth::XstsToken) -> Result<serde_json::Value> {
    request(session, "GET", "/worlds")
}

/// Every Realm the account can see, owned or joined.
pub fn list(session: &auth::XstsToken) -> Result<Vec<Realm>> {
    let body: serde_json::Value = request(session, "GET", "/worlds")?;
    let servers = body["servers"]
        .as_array()
        .context("the Realms service did not return a server list")?;
    Ok(servers
        .iter()
        .map(|v| realm_from_as(v, session.xuid.as_deref()))
        .collect())
}

#[cfg(test)]
fn realm_from(v: &serde_json::Value) -> Realm {
    realm_from_as(v, None)
}

/// Parse one Realm, deciding ownership by comparing against `my_xuid`.
fn realm_from_as(v: &serde_json::Value, my_xuid: Option<&str>) -> Realm {
    Realm {
        id: v["id"].as_i64().unwrap_or_default(),
        name: v["name"].as_str().unwrap_or("<unnamed>").to_owned(),
        motd: v["motd"].as_str().unwrap_or_default().to_owned(),
        state: v["state"].as_str().unwrap_or("UNKNOWN").to_owned(),
        // Ownership is `ownerUUID` against our own XUID, and nothing else.
        // `owner` is always empty on this endpoint, and `member` came back
        // false even for Realms the account had merely joined — an earlier
        // reading of `member` claimed every Realm was owned, which was wrong.
        owner: match (my_xuid, v["ownerUUID"].as_str()) {
            (Some(mine), Some(theirs)) if !theirs.is_empty() => Some(mine == theirs),
            _ => None,
        },
        owner_uuid: v["ownerUUID"].as_str().unwrap_or_default().to_owned(),
        expired: v["expired"].as_bool().unwrap_or(false),
        days_left: v["daysLeft"].as_i64(),
        active_slot: v["activeSlot"].as_i64(),
        player_count: v["players"].as_array().map(|p| p.len() as i64),
        max_players: v["maxPlayers"].as_i64(),
    }
}

/// One of a Realm's world slots.
#[derive(Debug, Clone, Serialize)]
pub struct Slot {
    pub slot_id: i64,
    /// The world's name as shown in game.
    pub name: Option<String>,
    pub game_mode: Option<String>,
    pub difficulty: Option<String>,
    pub hardcore: bool,
    pub seed: Option<String>,
    /// Marketplace pack ids this world uses, resource and behavior together.
    pub pack_ids: Vec<String>,
    /// Notable game rules, as label/value pairs for display.
    pub rules: Vec<(String, String)>,
    /// True when this is the slot the Realm is currently running.
    pub active: bool,
    /// No world has ever been put here.
    pub empty: bool,
}

/// Every Bedrock Realm has three world slots, whether or not they hold a world.
pub const SLOT_COUNT: i64 = 3;

/// A Realm with everything the detail endpoint knows about it.
#[derive(Debug, Clone, Serialize)]
pub struct RealmDetail {
    pub realm: Realm,
    pub slots: Vec<Slot>,
    pub players: Vec<Player>,
    /// Subscription product id, e.g. the 10-player monthly.
    pub product: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Player {
    pub uuid: String,
    pub name: Option<String>,
    pub role: Option<String>,
    pub permission: Option<String>,
    pub online: bool,
    pub last_login: Option<i64>,
}

/// Everything the service knows about one Realm.
pub fn detail(session: &auth::XstsToken, realm_id: i64) -> Result<RealmDetail> {
    let body = request(session, "GET", &format!("/worlds/{realm_id}"))?;
    let realm = realm_from_as(&body, session.xuid.as_deref());
    let active_slot = realm.active_slot;

    // The service reports only slots that have been used, but a Realm always
    // has three; the unused ones are shown as empty rather than hidden.
    let reported: Vec<Slot> = body["slots"]
        .as_array()
        .map(|list| list.iter().map(|s| slot_from(s, active_slot)).collect())
        .unwrap_or_default();
    let slots: Vec<Slot> = (1..=SLOT_COUNT)
        .map(|id| {
            reported
                .iter()
                .find(|s| s.slot_id == id)
                .cloned()
                .unwrap_or_else(|| Slot {
                    slot_id: id,
                    name: None,
                    game_mode: None,
                    difficulty: None,
                    hardcore: false,
                    seed: None,
                    pack_ids: Vec::new(),
                    rules: Vec::new(),
                    active: Some(id) == active_slot,
                    empty: true,
                })
        })
        .collect();

    let players = body["players"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|p| Player {
                    uuid: p["uuid"].as_str().unwrap_or_default().to_owned(),
                    name: non_empty(p["name"].as_str()),
                    role: non_empty(p["role"].as_str()),
                    permission: non_empty(p["permission"].as_str()),
                    online: p["online"].as_bool().unwrap_or(false),
                    last_login: p["lastLogin"].as_i64(),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(RealmDetail {
        realm,
        slots,
        players,
        product: non_empty(body["friendlyId"].as_str()),
    })
}

/// Game rules worth showing; the service returns dozens, most of them noise.
const NOTABLE_RULES: &[(&str, &str)] = &[
    ("keepinventory", "Keep inventory"),
    ("pvp", "Player vs player"),
    ("commandsEnabled", "Cheats"),
    ("showcoordinates", "Coordinates"),
    ("doDayLightCycle", "Day/night cycle"),
    ("dodaylightcycle", "Day/night cycle"),
    ("doweathercycle", "Weather"),
    ("domobspawning", "Mob spawning"),
    ("doimmediaterespawn", "Immediate respawn"),
    ("doinsomnia", "Phantoms"),
];

fn slot_from(v: &serde_json::Value, active_slot: Option<i64>) -> Slot {
    let slot_id = v["slotId"].as_i64().unwrap_or_default();

    // `options` is a JSON *string* holding the interesting fields.
    let options: serde_json::Value = v["options"]
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null);

    let settings: std::collections::HashMap<String, serde_json::Value> = v["settings"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|s| {
                    Some((s["name"].as_str()?.to_owned(), s["value"].clone()))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut pack_ids: Vec<String> = ["resourcePacks", "behaviorPacks"]
        .iter()
        .filter_map(|key| options["enabledPacks"][key].as_array())
        .flatten()
        .filter_map(|p| p.as_str().map(str::to_owned))
        .collect();
    pack_ids.sort();
    pack_ids.dedup();

    let rules = NOTABLE_RULES
        .iter()
        .filter_map(|(key, label)| {
            let value = settings.get(*key)?;
            let text = match value {
                serde_json::Value::Bool(b) => if *b { "on" } else { "off" }.to_owned(),
                other => other.as_i64().map(|n| n.to_string())?,
            };
            Some(((*label).to_owned(), text))
        })
        .collect::<Vec<_>>();

    Slot {
        name: non_empty(options["slotName"].as_str()),
        game_mode: game_mode_name(
            options["gameMode"].as_i64().or_else(|| settings.get("GameType")?.as_i64()),
        ),
        difficulty: difficulty_name(
            options["difficulty"].as_i64().or_else(|| settings.get("Difficulty")?.as_i64()),
        ),
        hardcore: options["hardcore"].as_bool().unwrap_or(false)
            || settings.get("IsHardcore").and_then(|v| v.as_bool()).unwrap_or(false),
        seed: settings.get("RandomSeed").and_then(|v| v.as_i64()).map(|n| n.to_string()),
        pack_ids,
        rules: dedupe_rules(rules),
        active: Some(slot_id) == active_slot,
        empty: false,
        slot_id,
    }
}

/// Make a slot the one the Realm runs.
pub fn switch_to_slot(session: &auth::XstsToken, realm_id: i64, slot: i64) -> Result<()> {
    request(session, "PUT", &format!("/worlds/{realm_id}/slot/{slot}")).map(|_| ())
}

/// Which slot the Realm is currently running.
fn state_active_slot(session: &auth::XstsToken, realm_id: i64) -> Result<Option<i64>> {
    let body = request(session, "GET", &format!("/worlds/{realm_id}"))?;
    Ok(body["activeSlot"].as_i64())
}

/// Switch slots and wait until the Realm reports it has actually moved.
///
/// Like closing, switching is asynchronous. Acting immediately afterwards —
/// asking to download the world, say — still hits the previous slot and the
/// service answers HTTP 500.
pub fn switch_and_wait(
    session: &auth::XstsToken,
    realm_id: i64,
    slot: i64,
    mut on_wait: impl FnMut(u64),
) -> Result<()> {
    const POLL: std::time::Duration = std::time::Duration::from_secs(4);
    const ATTEMPTS: u64 = 15;

    if let Err(e) = switch_to_slot(session, realm_id, slot) {
        let message = format!("{e:#}");
        if !message.contains("busy or unavailable") {
            return Err(e);
        }
    }
    for attempt in 1..=ATTEMPTS {
        if state_active_slot(session, realm_id)? == Some(slot) {
            // The world still needs a moment to be servable after the switch.
            std::thread::sleep(POLL);
            return Ok(());
        }
        std::thread::sleep(POLL);
        on_wait(attempt * POLL.as_secs());
    }
    bail!("the Realm did not switch to slot {slot} in time")
}

/// Two spellings of the same rule arrive from different parts of the payload.
fn dedupe_rules(rules: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut seen = std::collections::HashSet::new();
    rules.into_iter().filter(|(label, _)| seen.insert(label.clone())).collect()
}

fn game_mode_name(n: Option<i64>) -> Option<String> {
    Some(
        match n? {
            0 => "Survival",
            1 => "Creative",
            2 => "Adventure",
            _ => return None,
        }
        .to_owned(),
    )
}

fn difficulty_name(n: Option<i64>) -> Option<String> {
    Some(
        match n? {
            0 => "Peaceful",
            1 => "Easy",
            2 => "Normal",
            3 => "Hard",
            _ => return None,
        }
        .to_owned(),
    )
}

/// One stored backup of a Realm slot.
#[derive(Debug, Clone, Serialize)]
pub struct RealmBackup {
    pub backup_id: String,
    /// Unix seconds (the service reports milliseconds).
    pub last_modified: i64,
    pub size_bytes: u64,
    pub world_name: Option<String>,
    pub game_mode: Option<String>,
    pub difficulty: Option<String>,
    pub game_version: Option<String>,
}

/// Where to fetch a slot's world from, and the token that authorises it.
#[derive(Debug, Clone)]
pub struct SlotDownload {
    pub url: String,
    pub token: String,
    pub size_bytes: u64,
}

/// Backups the service holds for a Realm, newest first.
pub fn backups(session: &auth::XstsToken, realm_id: i64) -> Result<Vec<RealmBackup>> {
    let body = request(session, "GET", &format!("/worlds/{realm_id}/backups"))?;
    let list = body["backups"]
        .as_array()
        .context("the Realms service did not return a backup list")?;
    let mut out: Vec<RealmBackup> = list
        .iter()
        .map(|b| {
            let meta = &b["metadata"];
            RealmBackup {
                backup_id: b["backupId"].as_str().unwrap_or_default().to_owned(),
                last_modified: b["lastModifiedDate"].as_i64().unwrap_or(0) / 1000,
                size_bytes: b["size"].as_u64().unwrap_or(0),
                world_name: non_empty(meta["name"].as_str()),
                game_mode: non_empty(meta["game_mode"].as_str()),
                difficulty: non_empty(meta["game_difficulty"].as_str()),
                game_version: non_empty(meta["game_server_version"].as_str()),
            }
        })
        .collect();
    out.sort_by_key(|b| std::cmp::Reverse(b.last_modified));
    Ok(out)
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.filter(|v| !v.is_empty()).map(str::to_owned)
}

/// Ask where a slot's world can be downloaded from.
///
/// `backup_id` picks a specific stored backup; `None` means the slot as it
/// stands now.
pub fn slot_download(
    session: &auth::XstsToken,
    realm_id: i64,
    slot: i64,
    backup_id: Option<&str>,
) -> Result<SlotDownload> {
    let which = backup_id.unwrap_or("latest");
    let body = request(
        session,
        "GET",
        &format!("/archive/download/world/{realm_id}/{slot}/{which}"),
    )?;
    Ok(SlotDownload {
        url: body["downloadUrl"]
            .as_str()
            .context("the service did not return a download address")?
            .to_owned(),
        token: body["token"].as_str().unwrap_or_default().to_owned(),
        size_bytes: body["size"].as_u64().unwrap_or(0),
    })
}

/// Whether Minecraft will actually hand over this slot's world.
///
/// It often will not. The download serves Mojang's own stored copy, and a world
/// only just uploaded has none yet — the service answers HTTP 500 rather than
/// saying so. Worth checking before promising the user a backup that cannot be
/// taken. Asks for the address only; nothing is downloaded.
pub fn slot_is_downloadable(session: &auth::XstsToken, realm_id: i64, slot: i64) -> bool {
    slot_download(session, realm_id, slot, None).is_ok()
}

/// Stream a slot's world to disk as a `.mcworld`.
///
/// The content host is separate from the Realms API and takes the signed token
/// from [`slot_download`] rather than the Xbox credential.
pub fn fetch_world(
    download: &SlotDownload,
    dest: &std::path::Path,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<u64> {
    let mut request = ureq::get(&download.url).set("User-Agent", "MCPE/UWP");
    if !download.token.is_empty() {
        request = request.set("Authorization", &format!("Bearer {}", download.token));
    }

    let response = request.call().map_err(|e| match e {
        ureq::Error::Status(code, resp) => {
            let detail: String = resp.into_string().unwrap_or_default().chars().take(300).collect();
            anyhow!("the download failed with HTTP {code}: {detail}")
        }
        ureq::Error::Transport(t) => anyhow!("could not reach the download server: {t}"),
    })?;

    let expected = response
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(download.size_bytes);

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(dest)
        .with_context(|| format!("creating {}", dest.display()))?;
    let mut reader = response.into_reader();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut written = 0u64;
    loop {
        let n = std::io::Read::read(&mut reader, &mut buffer)?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buffer[..n])?;
        written += n as u64;
        on_progress(written, expected);
    }
    std::io::Write::flush(&mut file)?;

    if written == 0 {
        std::fs::remove_file(dest).ok();
        bail!("the download was empty");
    }
    Ok(written)
}

/// Close a Realm so its world can be replaced.
///
/// Returns as soon as the request is accepted; see [`close_and_wait`] for the
/// version that confirms the Realm actually went offline.
pub fn close(session: &auth::XstsToken, realm_id: i64) -> Result<()> {
    request(session, "PUT", &format!("/worlds/{realm_id}/close")).map(|_| ())
}

/// The Realm's current `OPEN`/`CLOSED` state.
pub fn state(session: &auth::XstsToken, realm_id: i64) -> Result<String> {
    let body = request(session, "GET", &format!("/worlds/{realm_id}"))?;
    Ok(body["state"].as_str().unwrap_or("UNKNOWN").to_owned())
}

/// Close a Realm and wait until the service confirms it is offline.
///
/// Closing is **asynchronous**: the service answers 503 "Retry again later"
/// while it shuts the server down, yet the close does happen. Taking that 503
/// at face value made the upload look permanently broken, when in truth it only
/// needed to wait and re-read the state.
pub fn close_and_wait(
    session: &auth::XstsToken,
    realm_id: i64,
    mut on_wait: impl FnMut(u64),
) -> Result<()> {
    const POLL: std::time::Duration = std::time::Duration::from_secs(4);
    const ATTEMPTS: u64 = 20;

    // A 503 here means "working on it", so it is not an error yet.
    if let Err(e) = close(session, realm_id) {
        let message = format!("{e:#}");
        if !message.contains("busy or unavailable") {
            return Err(e);
        }
    }

    for attempt in 1..=ATTEMPTS {
        std::thread::sleep(POLL);
        match state(session, realm_id)?.as_str() {
            "CLOSED" => return Ok(()),
            _ => on_wait(attempt * POLL.as_secs()),
        }
        // Nudge it again halfway through, in case the first call was dropped.
        if attempt == ATTEMPTS / 2 {
            let _ = close(session, realm_id);
        }
    }
    bail!("the Realm did not close in time — try again in a minute")
}

/// Reopen a Realm.
pub fn open(session: &auth::XstsToken, realm_id: i64) -> Result<()> {
    request(session, "PUT", &format!("/worlds/{realm_id}/open")).map(|_| ())
}

/// Send a `.mcworld` to a Realm slot.
///
/// As with downloads, the content host is separate from the API and takes the
/// signed token rather than the Xbox credential.
pub fn upload_world(
    session: &auth::XstsToken,
    realm_id: i64,
    slot: i64,
    mcworld: &std::path::Path,
) -> Result<()> {
    let info = request(
        session,
        "GET",
        &format!("/archive/upload/world/{realm_id}/{slot}"),
    )?;
    let url = info["uploadUrl"]
        .as_str()
        .context("the service did not return an upload address")?;
    let token = info["token"].as_str().unwrap_or_default();

    let bytes = std::fs::read(mcworld)
        .with_context(|| format!("reading {}", mcworld.display()))?;
    if bytes.is_empty() {
        bail!("refusing to upload an empty file");
    }

    // The verb is not documented anywhere; POST is what the service accepts,
    // with PUT tried only if it reports the method is wrong.
    match send_world(url, token, "POST", &bytes) {
        Err(UploadError::MethodNotAllowed) => match send_world(url, token, "PUT", &bytes) {
            Ok(()) => Ok(()),
            Err(e) => Err(e.into()),
        },
        Err(e) => Err(e.into()),
        Ok(()) => Ok(()),
    }
}

enum UploadError {
    MethodNotAllowed,
    Other(anyhow::Error),
}

impl From<UploadError> for anyhow::Error {
    fn from(e: UploadError) -> Self {
        match e {
            UploadError::MethodNotAllowed => anyhow!("the upload server rejected the request method"),
            UploadError::Other(e) => e,
        }
    }
}

fn send_world(url: &str, token: &str, method: &str, bytes: &[u8]) -> Result<(), UploadError> {
    let mut request = ureq::request(method, url)
        .set("User-Agent", "MCPE/UWP")
        .set("Content-Type", "application/octet-stream");
    if !token.is_empty() {
        request = request.set("Authorization", &format!("Bearer {token}"));
    }
    match request.send_bytes(bytes) {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(405, _)) => Err(UploadError::MethodNotAllowed),
        Err(ureq::Error::Status(code, resp)) => {
            let detail: String = resp.into_string().unwrap_or_default().chars().take(300).collect();
            Err(UploadError::Other(anyhow!(
                "the upload failed with HTTP {code}: {detail}"
            )))
        }
        Err(ureq::Error::Transport(t)) => Err(UploadError::Other(anyhow!(
            "could not reach the upload server: {t}"
        ))),
    }
}

/// Replace a Realm slot's world with one from the vault.
///
/// The only destructive thing this app does to somebody else's server, so the
/// order is fixed and not optional:
///
/// 1. **Download the Realm's current world into the vault first.** If anything
///    later goes wrong, what was on the Realm still exists locally.
/// 2. Close the Realm, so nobody is playing while the world is swapped.
/// 3. Upload.
/// 4. Reopen — attempted even when the upload fails, so a failure never leaves
///    the Realm shut.
pub struct Replacement<'a> {
    pub realm_id: i64,
    pub slot: i64,
    /// The vault world to put on the Realm.
    pub world_dir: &'a std::path::Path,
    pub stamp: &'a str,
    /// Close the Realm before uploading. Realms requires it; only testing has
    /// reason to skip it.
    pub close_first: bool,
    /// Download the slot's current world into the vault first.
    ///
    /// False only when there is nothing to save: an empty slot, or one whose
    /// world Minecraft will not serve (see [`slot_is_downloadable`]). Never
    /// skip it for a slot whose world can actually be fetched.
    pub backup_first: bool,
}

pub fn replace_slot_world(
    session: &auth::XstsToken,
    vault: &crate::vault::Vault,
    job: &Replacement<'_>,
    mut on_step: impl FnMut(&str),
    on_progress: impl FnMut(u64, u64),
) -> Result<Option<crate::vault::LibraryEntry>> {
    let Replacement { realm_id, slot, world_dir, stamp, close_first, backup_first } = *job;

    // Uploading makes a slot the live one, so remember what was playing.
    let was_active = state_active_slot(session, realm_id)?;

    let saved = if backup_first {
        on_step("Saving the Realm's current world to your vault");
        let backup = slot_download(session, realm_id, slot, None)
            .context("asking the Realm for its current world")?;
        let temp_backup = vault
            .exports_dir()
            .join(format!("realm-{realm_id}-slot{slot}-before-{stamp}.mcworld"));
        fetch_world(&backup, &temp_backup, on_progress)
            .context("downloading the Realm's current world")?;
        let entry = vault
            .import_mcworld(&temp_backup, stamp)
            .context("saving the Realm's current world into the vault")?;
        std::fs::remove_file(&temp_backup).ok();
        Some(entry)
    } else {
        // Only for a slot that holds nothing: there is no world to lose.
        None
    };

    on_step("Packing your world");
    let outgoing = vault
        .exports_dir()
        .join(format!("upload-{realm_id}-slot{slot}.mcworld"));
    crate::mcworld::pack(world_dir, &outgoing).context("packing the world to upload")?;

    let mut closed = false;
    if close_first {
        on_step("Closing the Realm");
        close_and_wait(session, realm_id, |secs| {
            on_step(&format!("Waiting for the Realm to close ({secs}s)"));
        })
        .context("closing the Realm")?;
        closed = true;
    }

    on_step("Uploading");
    let uploaded = upload_world(session, realm_id, slot, &outgoing);
    std::fs::remove_file(&outgoing).ok();

    // Reopen whatever happened, so a failed upload never leaves it closed.
    let reopened = if closed {
        on_step("Reopening the Realm");
        open(session, realm_id)
    } else {
        Ok(())
    };

    uploaded.context("uploading the world to the Realm")?;
    if let Err(e) = reopened {
        return Err(e).context(
            "the world uploaded, but the Realm could not be reopened — open it from the game",
        );
    }

    // Cosmetic, and never worth failing the upload over: the world is already
    // on the Realm by this point.
    on_step("Naming the world on the Realm");
    let name = crate::vault::world_display_name(world_dir).unwrap_or_else(|| "World".to_owned());
    let _ = describe_uploaded_slot(session, realm_id, slot, world_dir, &name);

    // Uploading makes a slot the live one. If the Realm was playing something
    // else before, put it back: replacing a spare slot should not drag everyone
    // off the world they were on.
    if let Some(original) = was_active {
        if original != slot {
            on_step("Putting the Realm back on the world it was playing");
            let _ = switch_to_slot(session, realm_id, original);
        }
    }

    Ok(saved)
}

/// Send a JSON body to a Realms path. Used for slot settings.
pub fn send_json(
    session: &auth::XstsToken,
    method: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value> {
    let url = format!("{REALMS_BASE}{path}");
    let response = ureq::request(method, &url)
        .set("Authorization", &session.authorization())
        .set("Client-Version", &client_version())
        .set("User-Agent", "MCPE/UWP")
        .set("Accept", "*/*")
        .set("Content-Type", "application/json")
        .send_json(body.clone());

    match response {
        Ok(ok) => Ok(ok.into_json().unwrap_or(serde_json::Value::Null)),
        Err(ureq::Error::Status(code, resp)) => {
            let text = resp.into_string().unwrap_or_default();
            let reason = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v["errorMsg"].as_str().map(str::to_owned))
                .unwrap_or_else(|| text.chars().take(300).collect());
            Err(anyhow!("HTTP {code}: {reason}"))
        }
        Err(ureq::Error::Transport(t)) => Err(anyhow!("could not reach Realms: {t}")),
    }
}

/// The raw `options` object for one slot, exactly as the service holds it.
///
/// Slot settings are written back whole, so any change must start from the
/// current values — writing a partial object would reset the game rules.
pub fn slot_options(
    session: &auth::XstsToken,
    realm_id: i64,
    slot: i64,
) -> Result<serde_json::Value> {
    let body = request(session, "GET", &format!("/worlds/{realm_id}"))?;
    let slots = body["slots"].as_array().context("the Realm reported no slots")?;
    let found = slots
        .iter()
        .find(|s| s["slotId"].as_i64() == Some(slot))
        .with_context(|| format!("the Realm has no slot {slot}"))?;
    // `options` is a JSON document stored as a string.
    found["options"]
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .context("could not read the slot's settings")
}

/// Write a slot's settings back, having started from [`slot_options`].
pub fn set_slot_options(
    session: &auth::XstsToken,
    realm_id: i64,
    slot: i64,
    options: &serde_json::Value,
) -> Result<()> {
    send_json(
        session,
        "POST",
        &format!("/worlds/{realm_id}/slot/{slot}"),
        options,
    )
    .map(|_| ())
}

/// Rename the world in a slot, leaving every other setting alone.
pub fn set_slot_name(
    session: &auth::XstsToken,
    realm_id: i64,
    slot: i64,
    name: &str,
) -> Result<()> {
    let mut options = slot_options(session, realm_id, slot)?;
    options["slotName"] = serde_json::Value::String(name.to_owned());
    set_slot_options(session, realm_id, slot, &options)
}

/// Turn off every add-on on a slot.
pub fn clear_slot_packs(session: &auth::XstsToken, realm_id: i64, slot: i64) -> Result<()> {
    let mut options = slot_options(session, realm_id, slot)?;
    options["enabledPacks"] = serde_json::json!({ "resourcePacks": [], "behaviorPacks": [] });
    set_slot_options(session, realm_id, slot, &options)
}

/// Name a slot after the world put on it, and declare the add-ons that world
/// carries.
///
/// Without this an uploaded world shows as "Unnamed world" with anonymous
/// add-ons: Realms keeps its own ids for packs attached in game, which appear
/// nowhere locally and cannot be resolved to names. Declaring the world's own
/// pack ids instead means the Realm afterwards reports ids this app *can* name,
/// because the world itself records them.
fn describe_uploaded_slot(
    session: &auth::XstsToken,
    realm_id: i64,
    slot: i64,
    world_dir: &std::path::Path,
    name: &str,
) -> Result<()> {
    let mut options = slot_options(session, realm_id, slot)?;
    options["slotName"] = serde_json::Value::String(name.to_owned());

    let (resource, behavior) = crate::packs::world_pack_refs(world_dir);
    let ids = |refs: Vec<crate::packs::PackRef>| -> Vec<serde_json::Value> {
        refs.into_iter()
            .map(|r| serde_json::Value::String(r.uuid.replace('-', "")))
            .collect()
    };
    options["enabledPacks"] = serde_json::json!({
        "resourcePacks": ids(resource),
        "behaviorPacks": ids(behavior),
    });

    set_slot_options(session, realm_id, slot, &options)
}

/// GET any Xbox service with the Xbox Live credential.
///
/// Xbox splits its services across hosts (profile, inventory, catalogue), each
/// wanting its own `x-xbl-contract-version`; this is how they get explored.
pub fn xbox_get(
    identity: &auth::XstsToken,
    url: &str,
    contract: &str,
) -> Result<serde_json::Value> {
    let response = ureq::get(url)
        .set("Authorization", &identity.authorization())
        .set("x-xbl-contract-version", contract)
        .set("Accept", "application/json")
        .set("Accept-Language", "en-GB")
        .call();

    match response {
        Ok(ok) => Ok(ok.into_json().unwrap_or(serde_json::Value::Null)),
        Err(ureq::Error::Status(code, resp)) => {
            let detail: String = resp.into_string().unwrap_or_default().chars().take(400).collect();
            Err(anyhow!("HTTP {code}: {detail}"))
        }
        Err(ureq::Error::Transport(t)) => Err(anyhow!("could not reach it: {t}")),
    }
}

/// An authenticated **GET** against any Realms path.
///
/// The API is unofficial and undocumented, so exploring it against a live
/// account is how its shape gets established. Restricted to GET so probing can
/// never change anything on Mojang's side.
pub fn get(session: &auth::XstsToken, path: &str) -> Result<serde_json::Value> {
    request(session, "GET", path)
}

/// One authenticated call to the Realms service.
fn request(session: &auth::XstsToken, method: &str, path: &str) -> Result<serde_json::Value> {
    let url = format!("{REALMS_BASE}{path}");
    // `Accept: application/json` makes the open/close endpoints answer 406:
    // they reply with a bare `true`, not JSON. Accept anything.
    let call = ureq::request(method, &url)
        .set("Authorization", &session.authorization())
        .set("Client-Version", &client_version())
        .set("User-Agent", "MCPE/UWP")
        .set("Accept", "*/*");

    // State-changing calls carry no payload, but must still say so: a PUT with
    // no Content-Length at all is not accepted.
    let response = if method == "GET" {
        call.call()
    } else {
        call.set("Content-Type", "application/json").send_string("")
    };

    match response {
        // Some endpoints (open/close) answer with a bare "true" or nothing at
        // all, which is success rather than a malformed reply.
        Ok(ok) => Ok(ok.into_json().unwrap_or(serde_json::Value::Null)),
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            // The service wraps its reason in {"errorCode":..,"errorMsg":".."}.
            let reason = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["errorMsg"].as_str().map(str::to_owned))
                .unwrap_or_else(|| body.chars().take(300).collect());
            Err(match code {
                401 => anyhow!(
                    "Realms would not accept the sign-in. Signing out and back in usually fixes it. ({reason})"
                ),
                // Authenticated but not allowed: on a Realm you have only
                // joined, downloading and uploading are the owner's alone.
                403 => anyhow!(
                    "Minecraft would not allow that — only a Realm's owner can download or replace its world. ({reason})"
                ),
                404 => anyhow!("Realms has no such Realm, slot or backup. ({reason})"),
                426 => anyhow!(
                    "Realms says this client version is too old — the version sent was {}. ({reason})",
                    client_version()
                ),
                503 => anyhow!("Realms is busy or unavailable right now. ({reason})"),
                _ => anyhow!("Realms returned HTTP {code}: {reason}"),
            })
        }
        Err(ureq::Error::Transport(t)) => Err(anyhow!("could not reach Realms: {t}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_version_is_three_parts() {
        let v = client_version();
        assert_eq!(
            v.split('.').count(),
            3,
            "Realms expects a three-part version, got '{v}'"
        );
        assert!(v.split('.').all(|p| p.parse::<u32>().is_ok()), "'{v}' must be numeric");
    }

    #[test]
    fn newest_version_wins_not_lexically_largest() {
        // "1.9.0" sorts after "1.21.0" as text; the numeric key must not.
        let mut versions = ["1.21.62.1.0", "1.9.0.0.0"];
        versions.sort_by_key(|v| version_key(v));
        assert_eq!(versions.last(), Some(&"1.21.62.1.0"));
    }

    /// Shaped from a real `/worlds` response: `owner` really does come back
    /// empty, and `member` is what distinguishes owned from joined.
    #[test]
    fn parses_a_realm_listing() {
        let body = serde_json::json!({
            "id": 12345, "name": "Example Realm", "motd": "",
            "state": "OPEN", "owner": "", "ownerUUID": "0000000000000000",
            "member": false, "expired": false, "daysLeft": 22, "activeSlot": 3,
            "players": null, "maxPlayers": 10, "tier": "TEN_PLAYERS"
        });
        let realm = realm_from_as(&body, Some("0000000000000000"));
        assert_eq!(realm.id, 12345);
        assert_eq!(realm.name, "Example Realm");
        assert_eq!(realm.state, "OPEN");
        assert_eq!(realm.days_left, Some(22));
        assert_eq!(realm.active_slot, Some(3));
        assert_eq!(realm.player_count, None, "players is null unless populated");
        assert_eq!(realm.owner, Some(true), "ownerUUID matches our XUID");
        assert_eq!(realm.subscription(), "22 days left");
    }

    /// The bug this replaced: `member` was false even for Realms the account
    /// had only joined, so reading it marked every Realm as owned.
    #[test]
    fn a_joined_realm_is_not_owned_even_when_member_is_false() {
        let body = serde_json::json!({
            "name": "Someone Else's", "ownerUUID": "1111111111111111", "member": false
        });
        let realm = realm_from_as(&body, Some("0000000000000000"));
        assert_eq!(realm.owner, Some(false));
        assert_eq!(realm.role(), "joined");
    }

    #[test]
    fn ownership_is_unknown_rather_than_guessed() {
        let body = serde_json::json!({ "name": "Any", "ownerUUID": "1111111111111111" });
        // No XUID for the signed-in account: must not claim either way, since
        // only an owner may replace a Realm's world.
        assert_eq!(realm_from_as(&body, None).owner, None);
        assert_eq!(realm_from_as(&body, None).role(), "?");
        // Owner not reported by the service either.
        let blank = serde_json::json!({ "name": "Any", "ownerUUID": "" });
        assert_eq!(realm_from_as(&blank, Some("0000000000000000")).owner, None);
    }

    #[test]
    fn expired_realms_read_as_expired() {
        let expired = realm_from(&serde_json::json!({ "expired": true, "daysLeft": -551 }));
        assert_eq!(expired.subscription(), "expired");
        // Some rows report a negative count without the flag.
        let lapsed = realm_from(&serde_json::json!({ "expired": false, "daysLeft": -1 }));
        assert_eq!(lapsed.subscription(), "expired");
    }

    #[test]
    fn missing_fields_do_not_panic() {
        let realm = realm_from(&serde_json::json!({}));
        assert_eq!(realm.name, "<unnamed>");
        assert_eq!(realm.state, "UNKNOWN");
        assert_eq!(realm.owner, None);
    }
}
