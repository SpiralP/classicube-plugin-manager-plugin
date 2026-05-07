#[cfg(test)]
mod tests;

use std::{env, result, time::Duration};

use anyhow::{Error, Result, anyhow};
use reqwest::{
    Client, StatusCode,
    header::{AUTHORIZATION, HeaderValue},
};
use serde::{Deserialize, Deserializer, de::Error as DeError};
use serde_json::Value;
use tracing::warn;

use crate::{config::Channel, installer::parse_sha256_digest};

const APP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

pub fn make_client() -> Client {
    Client::builder()
        .user_agent(APP_USER_AGENT)
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

#[derive(Debug, Deserialize)]
struct GitHubError {
    message: String,
}

#[derive(Debug, Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    #[serde(deserialize_with = "deserialize_iso8601_z")]
    pub published_at: u64,
    #[serde(default)]
    pub assets: Vec<GitHubReleaseAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubReleaseAsset {
    pub name: String,
    /// API URL for the asset; needed to download from private repos because
    /// `browser_download_url` is a web route that doesn't honor Bearer tokens.
    pub url: String,
    /// `"sha256:<hex>"` when GitHub publishes one. Older releases / older API
    /// responses omit this, so it stays optional.
    #[serde(default)]
    pub digest: Option<String>,
}

/// Fetch the appropriate release for `channel`:
/// - `Stable` hits `/releases/latest` (GitHub's "latest non-prerelease").
/// - `Prerelease` lists recent releases and picks the one with the latest
///   `published_at` — including prereleases. We don't filter on the
///   `prerelease` bit because users on this channel want the absolute newest
///   release on the timeline regardless of its label.
/// - `Tag(t)` hits `/releases/tags/{t}` directly.
///
/// `token` is the per-subscription PAT; falls back to `GITHUB_TOKEN` env var
/// when `None`. When neither is set the request goes anonymous (60/hr rate
/// limit, no access to private repos).
pub async fn get_release_for_channel(
    owner: &str,
    repo: &str,
    channel: &Channel,
    token: Option<&str>,
) -> Result<GitHubRelease> {
    match channel {
        Channel::Stable => fetch_stable_release(owner, repo, token).await,
        Channel::Prerelease => fetch_newest_release(owner, repo, token).await,
        Channel::Tag(tag) => {
            let url = format!("https://api.github.com/repos/{owner}/{repo}/releases/tags/{tag}");
            fetch_one(&url, token).await
        }
    }
}

async fn fetch_one(url: &str, token: Option<&str>) -> Result<GitHubRelease> {
    let bytes = send(url, token).await?;
    Ok::<_, Error>(serde_json::from_slice::<GitHubRelease>(&bytes)?)
}

/// Fetch the latest stable release. On 404, distinguishes "repo doesn't
/// exist / is private" from "repo exists but has no releases yet" (or has
/// only prereleases) by probing the list endpoint - the original 404
/// against `/releases/latest` doesn't tell those cases apart.
async fn fetch_stable_release(
    owner: &str,
    repo: &str,
    token: Option<&str>,
) -> Result<GitHubRelease> {
    let url = format!("https://api.github.com/repos/{owner}/{repo}/releases/latest");
    let resolved = resolve_auth_token(token);
    let had_token = resolved.is_some();
    let (status, body) = send_raw(&url, resolved.as_deref()).await?;
    if status.is_success() {
        return Ok(serde_json::from_slice::<GitHubRelease>(&body)?);
    }
    if status != StatusCode::NOT_FOUND {
        return Err(classify_error(status, had_token, &body));
    }

    let probe_url = format!("https://api.github.com/repos/{owner}/{repo}/releases?per_page=1");
    let (probe_status, probe_body) = send_raw(&probe_url, resolved.as_deref()).await?;
    Err(classify_stable_404_probe(
        owner,
        repo,
        probe_status,
        &probe_body,
        had_token,
    ))
}

/// Pure half of `fetch_stable_release`'s 404 fallback - takes the probe
/// response and shapes the chat-facing error. Extracted so it can be
/// unit-tested without spinning up an HTTP server.
pub(crate) fn classify_stable_404_probe(
    owner: &str,
    repo: &str,
    probe_status: StatusCode,
    probe_body: &[u8],
    had_token: bool,
) -> Error {
    if !probe_status.is_success() {
        // Probe also failed - repo really is missing or private. Defer to
        // the generic classifier so the token hint still appears for
        // anonymous 404s.
        return classify_error(probe_status, had_token, probe_body);
    }
    match serde_json::from_slice::<Value>(probe_body) {
        Ok(Value::Array(arr)) if arr.is_empty() => {
            anyhow!("{owner}/{repo} exists but has no published releases yet")
        }
        Ok(Value::Array(_)) => anyhow!(
            "{owner}/{repo} has no stable release (only prereleases); try `channel {owner}/{repo} \
             prerelease`"
        ),
        _ => anyhow!("HTTP {probe_status} from list-releases probe (unexpected body)"),
    }
}

async fn fetch_newest_release(
    owner: &str,
    repo: &str,
    token: Option<&str>,
) -> Result<GitHubRelease> {
    let url = format!("https://api.github.com/repos/{owner}/{repo}/releases?per_page=10");
    let bytes = send(&url, token).await?;
    let releases: Vec<GitHubRelease> = serde_json::from_slice(&bytes)?;
    releases
        .into_iter()
        .max_by_key(|r| r.published_at)
        .ok_or_else(|| anyhow!("no releases found for {owner}/{repo}"))
}

/// Resolve the effective auth token for a request. Per-subscription wins;
/// `GITHUB_TOKEN` env var is a global fallback. Returns `None` when neither
/// is set, in which case the request goes anonymous.
pub(crate) fn resolve_auth_token(per_sub: Option<&str>) -> Option<String> {
    per_sub
        .map(str::to_owned)
        .or_else(|| env::var("GITHUB_TOKEN").ok())
}

/// Send a GET request and return the body bytes for any 2xx response. Maps
/// non-success statuses to an `anyhow::Error` whose message is shaped for
/// chat output — including a hint when an anonymous 404 is likely a private
/// repo, and when an authed 401/403 likely means a stale token.
pub(crate) async fn send(url: &str, token: Option<&str>) -> Result<Vec<u8>> {
    let resolved = resolve_auth_token(token);
    let had_token = resolved.is_some();
    let (status, body) = send_raw(url, resolved.as_deref()).await?;
    if status.is_success() {
        return Ok(body);
    }
    Err(classify_error(status, had_token, &body))
}

/// Send a GET and return `(status, body)` for any HTTP response - including
/// non-2xx. Transport / auth-header construction failures still bubble up
/// as errors. Callers that need to react to a specific status (e.g. probe
/// on 404) use this; callers that just want the body use `send`.
///
/// Token handling differs from `send`: this takes the already-resolved
/// token (so callers chaining multiple requests can resolve once and reuse
/// the same effective auth across both).
pub(crate) async fn send_raw(url: &str, token: Option<&str>) -> Result<(StatusCode, Vec<u8>)> {
    let mut request = make_client().get(url);
    if let Some(t) = token {
        let mut header_value = HeaderValue::from_str(&format!("Bearer {t}"))
            .map_err(|e| anyhow!("invalid token characters: {e}"))?;
        header_value.set_sensitive(true);
        request = request.header(AUTHORIZATION, header_value);
    }

    let resp = request.send().await?;
    let status = resp.status();
    let body = resp.bytes().await?.to_vec();
    Ok((status, body))
}

/// Map a non-success GitHub response to a chat-friendly error. Extracted so
/// it can be unit-tested without spinning up an HTTP server.
pub(crate) fn classify_error(status: StatusCode, had_token: bool, body: &[u8]) -> Error {
    let api_msg = serde_json::from_slice::<GitHubError>(body)
        .ok()
        .map(|e| e.message);

    match status {
        StatusCode::NOT_FOUND if !had_token => anyhow!(
            "not found (if this repo is private, retry with `add <owner>/<repo> token \
             github_pat_...` or add `token = \"github_pat_...\"` to its entry in \
             plugin-manager.toml)"
        ),
        // 404 with a token attached has two real causes: repo is gone, or
        // the token can't see it. Fine-grained PATs return 404 (not 403)
        // for repos they aren't scoped to, so the second case is silent
        // unless we spell it out.
        StatusCode::NOT_FOUND if had_token => anyhow!(
            "not found ({}) - either the repo is missing/renamed, or your token lacks `Contents: \
             Read` access to it (fine-grained PATs return 404 for repos they can't see)",
            api_msg.unwrap_or_else(|| "Not Found".to_string())
        ),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN if had_token => anyhow!(
            "auth failed (token may be expired or lack `Contents: Read` on this repo): {}",
            api_msg.unwrap_or_else(|| status.to_string())
        ),
        _ => match api_msg {
            Some(m) => anyhow!("{m}"),
            None => anyhow!("HTTP {status}"),
        },
    }
}

/// Parse GitHub's release timestamp format (`YYYY-MM-DDTHH:MM:SSZ`, RFC3339
/// in UTC) into unix seconds. Strict — anything else returns `None`.
fn parse_iso8601_z(s: &str) -> Option<u64> {
    if s.len() != 20 {
        return None;
    }
    let b = s.as_bytes();
    if b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let parse = |start: usize, end: usize| s[start..end].parse::<u32>().ok();
    let year = parse(0, 4)?;
    let month = parse(5, 7)?;
    let day = parse(8, 10)?;
    let hour = parse(11, 13)?;
    let minute = parse(14, 16)?;
    let second = parse(17, 19)?;

    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour >= 24
        || minute >= 60
        || second >= 60
    {
        return None;
    }

    let is_leap = |y: u32| (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400);
    let dim: [u32; 12] = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    if day > dim[(month - 1) as usize] {
        return None;
    }

    let mut days: u64 = 0;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    for m in 1..month {
        days += u64::from(dim[(m - 1) as usize]);
    }
    days += u64::from(day - 1);

    Some(days * 86_400 + u64::from(hour) * 3600 + u64::from(minute) * 60 + u64::from(second))
}

fn deserialize_iso8601_z<'de, D>(d: D) -> result::Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    parse_iso8601_z(&s).ok_or_else(|| DeError::custom(format!("invalid ISO-8601-Z timestamp: {s}")))
}

/// Pick which SHA-256 digest to enforce on the upcoming download.
///
/// GitHub's per-asset `digest` field is authoritative — if it's present but
/// malformed, that's a real problem (API change / MITM / bug) and we hard
/// fail rather than silently skip. Absent is fine — older releases / older
/// API responses omit it and we don't want to block their updates.
pub fn resolve_expected_digest(asset: &GitHubReleaseAsset) -> Result<Option<String>> {
    match asset.digest.as_deref() {
        Some(d) => {
            parse_sha256_digest(d)?;
            Ok(Some(d.to_owned()))
        }
        None => {
            warn!(
                "no published digest for {}; skipping verification",
                asset.name
            );
            Ok(None)
        }
    }
}
