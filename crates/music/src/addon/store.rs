//! The addon servers the listener has added, in priority order.
//!
//! One JSON file under the provider's own accounts folder, written atomically through
//! `credentials::write` so a crash never leaves half a list. A missing or malformed file is an
//! empty list rather than an error: the provider then has nothing to offer, which is a state
//! the accounts screen explains.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::credentials;

/// The folder and file every addon lives in, under the accounts root.
const SLUG: &str = "addons";
const FILE: &str = "addons.json";

/// One configured addon server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The base URL, as the client wants it: no trailing slash, no `manifest.json`.
    pub url: String,
    /// What to show for it. Empty until a manifest has been read; the host is the fallback.
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

// `new`, `display_name`, `save` and `enabled` are for the settings screen that manages the
// list; nothing else writes the file yet.
#[allow(dead_code)]
impl Entry {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            name: String::new(),
            enabled: true,
        }
    }

    /// The name to show when the manifest never gave one.
    pub fn display_name(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.clone();
        }
        reqwest::Url::parse(&self.url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_owned))
            .unwrap_or_else(|| self.url.clone())
    }
}

pub fn path() -> PathBuf {
    credentials::dir(SLUG).join(FILE)
}

/// The configured addons, in the order they are tried. An unreadable file is an empty list,
/// since nothing the listener added can be recovered from it anyway and the screen offers to
/// add again.
pub fn load() -> Vec<Entry> {
    let path = path();
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<Entry>>(&contents) {
        Ok(entries) => entries
            .into_iter()
            .filter(|entry| !entry.url.trim().is_empty())
            .collect(),
        Err(error) => {
            log::warn!("addons: ignoring an unreadable list at {path:?}: {error}");
            Vec::new()
        }
    }
}

#[allow(dead_code)]
pub fn save(entries: &[Entry]) -> Result<()> {
    let path = path();
    let bytes = serde_json::to_vec_pretty(entries).context("cannot write the addon list")?;
    credentials::write(&path, &bytes)
}

#[allow(dead_code)]
pub fn enabled(entries: &[Entry]) -> impl Iterator<Item = (usize, &Entry)> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.enabled)
}
