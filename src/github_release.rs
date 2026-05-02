use std::{env, time::Duration};

use anyhow::{Error, Result, bail};
use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde::Deserialize;

const APP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

fn make_client() -> reqwest::Client {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_release() {
        let json = br#"{"tag_name":"v1.0.0"}"#;
        let r: GitHubRelease = serde_json::from_slice(json).unwrap();
        assert_eq!(r.tag_name, "v1.0.0");
    }

    #[test]
    fn ignores_extra_release_fields() {
        // Trimmed shape of an actual GitHub /releases/latest payload —
        // confirms that the many fields we don't model don't break parsing.
        let json = br#"{
            "url": "https://api.github.com/repos/o/r/releases/1",
            "id": 1,
            "tag_name": "v2.3.4",
            "name": "v2.3.4",
            "draft": false,
            "prerelease": false,
            "created_at": "2026-01-01T00:00:00Z",
            "published_at": "2026-01-01T00:00:00Z",
            "assets": [
                {"name": "plugin.so", "browser_download_url": "https://example/plugin.so"},
                {"name": "plugin.dll", "browser_download_url": "https://example/plugin.dll"}
            ],
            "body": "Release notes here."
        }"#;
        let r: GitHubRelease = serde_json::from_slice(json).unwrap();
        assert_eq!(r.tag_name, "v2.3.4");
    }

    #[test]
    fn parses_error_payload() {
        let json = br#"{"message":"Not Found","documentation_url":"https://docs"}"#;
        let e: GitHubError = serde_json::from_slice(json).unwrap();
        assert_eq!(e.message, "Not Found");
    }

    #[test]
    fn release_payload_does_not_match_error_shape() {
        // Sanity check: the success-path body does not accidentally deserialize
        // into GitHubError (the get_latest_release flow tries error first).
        let json = br#"{"tag_name":"v1.0.0"}"#;
        assert!(serde_json::from_slice::<GitHubError>(json).is_err());
    }
}
