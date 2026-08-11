//! The signed-in player's Xbox profile: gamertag and profile picture.
//!
//! Uses the Xbox Live credential (relying party `http://xboxlive.com`), not the
//! Realms one — the Realms token carries no identity at all.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use super::auth::XstsToken;

const PROFILE_URL: &str = "https://profile.xboxlive.com/users/me/profile/settings";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub gamertag: String,
    /// Profile picture as a `data:` URI.
    ///
    /// Inlined rather than linked because the webview's content policy blocks
    /// remote images, and so the picture still shows when offline.
    pub picture: Option<String>,
    /// Gamerscore, when Xbox reports it.
    pub gamerscore: Option<String>,
}

/// Read the signed-in account's profile.
pub fn fetch(identity: &XstsToken) -> Result<Profile> {
    let settings = "Gamertag,GameDisplayPicRaw,Gamerscore,ModernGamertag";
    let response = ureq::get(&format!("{PROFILE_URL}?settings={settings}"))
        .set("Authorization", &identity.authorization())
        .set("x-xbl-contract-version", "3")
        .set("Accept", "application/json")
        .set("Accept-Language", "en-GB")
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, resp) => {
                let detail: String =
                    resp.into_string().unwrap_or_default().chars().take(200).collect();
                anyhow!("the Xbox profile service returned HTTP {code}: {detail}")
            }
            ureq::Error::Transport(t) => anyhow!("could not reach the Xbox profile service: {t}"),
        })?;

    let body: serde_json::Value = response.into_json().context("reading the Xbox profile")?;
    let settings = body["profileUsers"][0]["settings"]
        .as_array()
        .context("the Xbox profile had no settings")?;

    let get = |id: &str| -> Option<String> {
        settings
            .iter()
            .find(|s| s["id"].as_str() == Some(id))
            .and_then(|s| s["value"].as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };

    let gamertag = get("ModernGamertag")
        .or_else(|| get("Gamertag"))
        .or_else(|| identity.gamertag.clone())
        .unwrap_or_else(|| "Signed in".to_owned());

    Ok(Profile {
        gamertag,
        picture: get("GameDisplayPicRaw").and_then(|url| fetch_image(&url).ok()),
        gamerscore: get("Gamerscore"),
    })
}

/// Download the player's picture at avatar size.
///
/// Xbox serves the raw picture at whatever size is asked for; 128px is plenty
/// for a header avatar and keeps the cached copy small.
pub fn fetch_image(url: &str) -> Result<String> {
    let sized = if url.contains('?') {
        format!("{url}&format=png&w=128&h=128")
    } else {
        format!("{url}?format=png&w=128&h=128")
    };
    fetch_data_uri(&sized)
}

/// Download an image and return it as a `data:` URI.
///
/// Everything shown in the window is inlined like this: the webview's content
/// policy blocks remote images, and it means a picture already on screen keeps
/// working offline. Capped so a surprising response cannot bloat the page or
/// the saved session file.
pub fn fetch_data_uri(url: &str) -> Result<String> {
    use base64::Engine;

    const MAX_BYTES: usize = 2 * 1024 * 1024;

    let response = ureq::get(url)
        .call()
        .map_err(|e| anyhow!("could not download the picture: {e}"))?;
    let content_type = response
        .header("Content-Type")
        .unwrap_or("image/png")
        .to_owned();

    let mut bytes = Vec::new();
    let mut reader = std::io::Read::take(response.into_reader(), MAX_BYTES as u64);
    std::io::Read::read_to_end(&mut reader, &mut bytes)?;
    if bytes.is_empty() {
        return Err(anyhow!("the picture was empty"));
    }

    Ok(format!(
        "data:{content_type};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}
