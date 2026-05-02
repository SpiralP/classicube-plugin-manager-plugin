#[cfg(test)]
mod tests;

use std::{env, time::Duration};

use anyhow::{Error, Result, bail};
use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde::Deserialize;

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
