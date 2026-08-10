//! Microsoft sign-in for Bedrock Realms.
//!
//! Four hops, all plain HTTPS + JSON:
//!
//! 1. **Device code** — ask Microsoft for a short code the user types at
//!    `microsoft.com/link`, then poll until they finish.
//! 2. **Xbox Live** — exchange the Microsoft token for an XBL user token.
//! 3. **XSTS** — authorise that user token against the Realms relying party.
//! 4. The result is `XBL3.0 x=<user hash>;<token>`, the Realms `Authorization`.
//!
//! Tokens are cached locally (see [`super::cache`]) and never logged: errors
//! quote the service's own message, never the token.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Public client id of the Minecraft (Nintendo Switch) title.
///
/// Bedrock Realms accepts tokens minted for this title; it is the id the
/// community clients use, and it needs the older `login.live.com` endpoints
/// rather than the Azure AD ones.
const CLIENT_ID: &str = "00000000441cc96b";
const SCOPE: &str = "service::user.auth.xboxlive.com::MBI_SSL";

const DEVICE_CODE_URL: &str = "https://login.live.com/oauth20_connect.srf";
const TOKEN_URL: &str = "https://login.live.com/oauth20_token.srf";
const XBL_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
/// The Realms service is the relying party the XSTS token must be scoped to.
pub const REALMS_RELYING_PARTY: &str = "https://pocket.realms.minecraft.net/";

/// What the user must do to finish signing in.
#[derive(Debug, Clone)]
pub struct DeviceLogin {
    pub user_code: String,
    pub verification_uri: String,
    /// Opaque handle used when polling; not shown to the user.
    pub device_code: String,
    pub interval_secs: u64,
    pub expires_in_secs: u64,
}

/// Microsoft account tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MicrosoftTokens {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix time this access token stops working.
    pub expires_at: i64,
}

/// An authorised Realms session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XstsToken {
    pub token: String,
    /// Xbox user hash, the `x=` part of the header.
    pub user_hash: String,
    /// Gamertag, when Xbox supplies one.
    pub gamertag: Option<String>,
    pub expires_at: i64,
}

impl XstsToken {
    /// The `Authorization` header value Realms expects.
    pub fn authorization(&self) -> String {
        format!("XBL3.0 x={};{}", self.user_hash, self.token)
    }

    pub fn is_expired(&self) -> bool {
        // Treat nearly-expired as expired: a request must not die mid-flight.
        now() + 60 >= self.expires_at
    }
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Ask Microsoft for a device code for the user to enter.
pub fn start_device_login() -> Result<DeviceLogin> {
    let body: serde_json::Value = ureq::post(DEVICE_CODE_URL)
        .send_form(&[
            ("client_id", CLIENT_ID),
            ("scope", SCOPE),
            ("response_type", "device_code"),
        ])
        .map_err(describe)
        .context("asking Microsoft for a sign-in code")?
        .into_json()
        .context("reading the sign-in code response")?;

    Ok(DeviceLogin {
        user_code: string_field(&body, "user_code")?,
        verification_uri: string_field(&body, "verification_uri")
            .unwrap_or_else(|_| "https://www.microsoft.com/link".to_owned()),
        device_code: string_field(&body, "device_code")?,
        interval_secs: body["interval"].as_u64().unwrap_or(5).max(1),
        expires_in_secs: body["expires_in"].as_u64().unwrap_or(900),
    })
}

/// One poll of the device-code flow.
///
/// `Ok(None)` means the user has not finished yet and the caller should wait.
pub fn poll_device_login(device_code: &str) -> Result<Option<MicrosoftTokens>> {
    let response = ureq::post(TOKEN_URL).send_form(&[
        ("client_id", CLIENT_ID),
        ("device_code", device_code),
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
    ]);

    match response {
        Ok(ok) => {
            let body: serde_json::Value =
                ok.into_json().context("reading the sign-in response")?;
            Ok(Some(tokens_from(&body)?))
        }
        Err(ureq::Error::Status(_, resp)) => {
            let body: serde_json::Value = resp.into_json().unwrap_or_default();
            match body["error"].as_str().unwrap_or("") {
                // Still waiting for the user, or told to slow down.
                "authorization_pending" | "slow_down" => Ok(None),
                "expired_token" => bail!("the sign-in code expired — start again"),
                "authorization_declined" => bail!("sign-in was declined"),
                other => bail!(
                    "sign-in failed: {}",
                    body["error_description"].as_str().unwrap_or(other)
                ),
            }
        }
        Err(e) => Err(anyhow!(describe(e))).context("polling for sign-in"),
    }
}

/// Swap a refresh token for a fresh access token.
pub fn refresh(refresh_token: &str) -> Result<MicrosoftTokens> {
    let body: serde_json::Value = ureq::post(TOKEN_URL)
        .send_form(&[
            ("client_id", CLIENT_ID),
            ("scope", SCOPE),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .map_err(describe)
        .context("refreshing the Microsoft sign-in")?
        .into_json()
        .context("reading the refresh response")?;
    tokens_from(&body)
}

/// Exchange a Microsoft access token for an XBL user token.
fn xbox_user_token(access_token: &str) -> Result<String> {
    let body: serde_json::Value = ureq::post(XBL_URL)
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .set("x-xbl-contract-version", "1")
        .send_json(serde_json::json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                // Live (MBI_SSL) tokens are sent as-is; the Azure AD flow
                // would need a "d=" prefix here.
                "RpsTicket": access_token,
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT",
        }))
        .map_err(describe)
        .context("signing in to Xbox Live")?
        .into_json()
        .context("reading the Xbox Live response")?;
    string_field(&body, "Token")
}

/// Authorise an XBL user token for the Realms service.
pub fn xsts_for_realms(user_token: &str) -> Result<XstsToken> {
    let response = ureq::post(XSTS_URL)
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .set("x-xbl-contract-version", "1")
        .send_json(serde_json::json!({
            "Properties": { "SandboxId": "RETAIL", "UserTokens": [user_token] },
            "RelyingParty": REALMS_RELYING_PARTY,
            "TokenType": "JWT",
        }));

    let body: serde_json::Value = match response {
        Ok(ok) => ok.into_json().context("reading the Xbox authorisation")?,
        Err(ureq::Error::Status(401, resp)) => {
            // XErr codes are the only useful signal Xbox gives here.
            let body: serde_json::Value = resp.into_json().unwrap_or_default();
            let hint = match body["XErr"].as_i64().unwrap_or(0) {
                2148916233 => "this Microsoft account has no Xbox profile — sign in to Minecraft or Xbox once first",
                2148916235 => "Xbox Live is not available in this account's country",
                2148916236 | 2148916237 => "this account needs adult verification",
                2148916238 => "this is a child account and must be added to a family group",
                _ => "Xbox Live refused the sign-in",
            };
            bail!("{hint}");
        }
        Err(e) => return Err(anyhow!(describe(e))).context("authorising with Xbox"),
    };

    let claims = &body["DisplayClaims"]["xui"][0];
    Ok(XstsToken {
        token: string_field(&body, "Token")?,
        user_hash: claims["uhs"]
            .as_str()
            .context("Xbox did not return a user hash")?
            .to_owned(),
        gamertag: claims["gtg"].as_str().map(str::to_owned),
        // Xbox returns an ISO timestamp; fall back to a conservative window.
        expires_at: body["NotAfter"]
            .as_str()
            .and_then(parse_iso8601)
            .unwrap_or_else(|| now() + 12 * 3600),
    })
}

/// Full chain: Microsoft access token to a Realms-ready XSTS token.
pub fn realms_session(access_token: &str) -> Result<XstsToken> {
    let user_token = xbox_user_token(access_token)?;
    xsts_for_realms(&user_token)
}

/// How long to wait between polls.
pub fn poll_delay(login: &DeviceLogin) -> Duration {
    Duration::from_secs(login.interval_secs)
}

fn tokens_from(body: &serde_json::Value) -> Result<MicrosoftTokens> {
    Ok(MicrosoftTokens {
        access_token: string_field(body, "access_token")?,
        refresh_token: string_field(body, "refresh_token")?,
        expires_at: now() + body["expires_in"].as_i64().unwrap_or(3600),
    })
}

fn string_field(body: &serde_json::Value, key: &str) -> Result<String> {
    body[key]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("response had no '{key}'"))
}

/// Describe a transport/HTTP failure without ever quoting the request body,
/// which would leak tokens into logs.
fn describe(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Status(code, resp) => {
            let detail = resp.into_string().unwrap_or_default();
            let detail = detail.chars().take(300).collect::<String>();
            anyhow!("service returned HTTP {code}: {detail}")
        }
        ureq::Error::Transport(t) => anyhow!("could not reach the service: {t}"),
    }
}

/// Minimal ISO-8601 parse for Xbox's `NotAfter` (e.g. `2026-08-11T09:14:22.5Z`).
fn parse_iso8601(text: &str) -> Option<i64> {
    let date = text.get(0..10)?;
    let time = text.get(11..19)?;
    let mut d = date.split('-');
    let (y, m, day) = (
        d.next()?.parse::<i64>().ok()?,
        d.next()?.parse::<i64>().ok()?,
        d.next()?.parse::<i64>().ok()?,
    );
    let mut t = time.split(':');
    let (hh, mm, ss) = (
        t.next()?.parse::<i64>().ok()?,
        t.next()?.parse::<i64>().ok()?,
        t.next()?.parse::<i64>().ok()?,
    );
    // Days since the Unix epoch, via the civil-from-days algorithm.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_header_is_the_shape_realms_expects() {
        let token = XstsToken {
            token: "eyJhbGciOi".into(),
            user_hash: "1234567890".into(),
            gamertag: Some("Someone".into()),
            expires_at: now() + 3600,
        };
        assert_eq!(token.authorization(), "XBL3.0 x=1234567890;eyJhbGciOi");
        assert!(!token.is_expired());
    }

    #[test]
    fn expiry_has_a_safety_margin() {
        let almost = XstsToken {
            token: "t".into(),
            user_hash: "h".into(),
            gamertag: None,
            // Inside the 60s margin, so it must already count as expired.
            expires_at: now() + 30,
        };
        assert!(almost.is_expired());
    }

    #[test]
    fn parses_xbox_timestamps() {
        // 20675 days from the epoch to 2026-08-10, plus 12h.
        assert_eq!(parse_iso8601("2026-08-10T12:00:00.1234567Z"), Some(1_786_363_200));
        // Epoch itself, and a leap day, as checks on the day arithmetic.
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso8601("2024-02-29T00:00:00Z"), Some(1_709_164_800));
        assert_eq!(parse_iso8601("nonsense"), None);
    }
}
