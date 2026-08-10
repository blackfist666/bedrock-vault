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

use anyhow::{anyhow, Context, Result};
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
    let token = auth::realms_session(&refreshed.access_token)?;
    saved.microsoft = Some(refreshed);
    saved.realms = Some(token.clone());
    cache::save(&saved)?;
    Ok(token)
}

/// Finish a device-code sign-in and save the session.
pub fn complete_login(tokens: auth::MicrosoftTokens) -> Result<auth::XstsToken> {
    let token = auth::realms_session(&tokens.access_token)?;
    cache::save(&cache::Session {
        microsoft: Some(tokens),
        realms: Some(token.clone()),
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

/// One authenticated call to the Realms service.
fn request(session: &auth::XstsToken, method: &str, path: &str) -> Result<serde_json::Value> {
    let url = format!("{REALMS_BASE}{path}");
    let response = ureq::request(method, &url)
        .set("Authorization", &session.authorization())
        .set("Client-Version", &client_version())
        .set("User-Agent", "MCPE/UWP")
        .set("Accept", "application/json")
        .call();

    match response {
        Ok(ok) => ok.into_json().context("reading the Realms response"),
        Err(ureq::Error::Status(code, resp)) => {
            let detail = resp.into_string().unwrap_or_default();
            let detail: String = detail.chars().take(400).collect();
            Err(match code {
                401 | 403 => anyhow!(
                    "Realms refused the sign-in (HTTP {code}). Signing out and back in usually fixes it. {detail}"
                ),
                426 => anyhow!(
                    "Realms says this client version is too old — the version sent was {}. {detail}",
                    client_version()
                ),
                _ => anyhow!("Realms returned HTTP {code}: {detail}"),
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
