#[cfg(test)]
mod tests;

use std::{env, time::Duration};

use anyhow::{Error, Result, bail};
use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde::{Deserialize, Deserializer};

const APP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

pub fn make_client() -> reqwest::Client {
    reqwest::Client::builder()
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
    pub browser_download_url: String,
}

pub async fn get_latest_release(owner: &str, repo: &str) -> Result<GitHubRelease> {
    let mut request = make_client().get(format!(
        "https://api.github.com/repos/{owner}/{repo}/releases/latest"
    ));
    if let Ok(token) = env::var("GITHUB_TOKEN") {
        let mut header_value = HeaderValue::from_str(&format!("token {token}")).unwrap();
        header_value.set_sensitive(true);
        request = request.header(AUTHORIZATION, header_value);
    }

    let bytes = request.send().await?.bytes().await?;

    if let Ok(error) = serde_json::from_slice::<GitHubError>(&bytes) {
        bail!("{}", error.message);
    }
    Ok::<_, Error>(serde_json::from_slice::<GitHubRelease>(&bytes)?)
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

fn deserialize_iso8601_z<'de, D>(d: D) -> std::result::Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    parse_iso8601_z(&s)
        .ok_or_else(|| serde::de::Error::custom(format!("invalid ISO-8601-Z timestamp: {s}")))
}
