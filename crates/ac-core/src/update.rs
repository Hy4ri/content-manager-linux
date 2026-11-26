//! Checks GitHub for a newer cm-rs release. Updating itself is left to the
//! user: the app only links to the release page.

use std::time::Duration;

use anyhow::{Context, Result, bail};

/// `owner/name` of the GitHub repository that publishes cm-rs releases.
pub const GITHUB_REPO: &str = "Hy4ri/content-manager-linux";

const TIMEOUT: Duration = Duration::from_secs(10);
/// The `releases/latest` JSON is a few KB; anything far larger is not GitHub.
const BODY_LIMIT: u64 = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Tag as published, e.g. `v3.1.0`.
    pub tag: String,
    /// Release page on github.com.
    pub url: String,
}

/// The newest published release (GitHub skips drafts and pre-releases here)
/// when it is newer than `current`, `None` when `current` is up to date.
pub fn check(current: &str) -> Result<Option<Release>> {
    let latest = latest_release()?;
    let (Some(new), Some(cur)) = (parse_version(&latest.tag), parse_version(current)) else {
        bail!("can't compare versions {current:?} and {:?}", latest.tag);
    };
    Ok((new > cur).then_some(latest))
}

fn latest_release() -> Result<Release> {
    let api = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .user_agent(concat!("cm-rs/", env!("CARGO_PKG_VERSION")))
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
    let body = agent
        .get(&api)
        .header("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| format!("requesting {api}"))?
        .body_mut()
        .with_config()
        .limit(BODY_LIMIT)
        .read_to_string()
        .with_context(|| format!("reading {api}"))?;
    let json: serde_json::Value = serde_json::from_str(&body).context("parsing the latest release")?;
    let Some(tag) = json["tag_name"].as_str() else {
        bail!("the latest release has no tag_name");
    };
    // The tag goes into a URL we open in the browser, so only accept plain
    // version-like tags instead of trusting `html_url` from the network.
    if tag.is_empty() || !tag.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+')) {
        bail!("unexpected release tag {tag:?}");
    }
    Ok(Release {
        tag: tag.to_owned(),
        url: format!("https://github.com/{GITHUB_REPO}/releases/tag/{tag}"),
    })
}

/// `v3.0.1`, `3.0.1` or `3.1` → `[3, 0, 1]`; any `-pre`/`+build` suffix is ignored.
fn parse_version(s: &str) -> Option<[u64; 3]> {
    let core = s.trim().trim_start_matches(['v', 'V']);
    let core = core.split(['-', '+']).next()?;
    let mut out = [0; 3];
    let mut parts = core.split('.');
    for slot in &mut out {
        match parts.next() {
            Some(p) => *slot = p.parse().ok()?,
            None => break,
        }
    }
    if parts.next().is_some() || out == [0; 3] {
        return None;
    }
    Some(out)
}
