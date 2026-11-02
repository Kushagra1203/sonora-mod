//! One addon server, and every question this provider knows how to ask it.
//!
//! Three endpoints, all GETs returning JSON, none of them executing anything on this side:
//! `/manifest.json` describes the addon, `/search?q=` finds tracks, `/stream/{id}` resolves
//! one to a URL. A miss (404 on `/stream`) is `Ok(None)` and walks to the next source; every
//! other failure is an error the caller logs and walks past the same way.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use reqwest::Url;

use super::wire::{AddonStream, AddonTrack, Manifest, SearchResponse};

/// The user agent every addon sees. Says what is asking, not which engine it embeds.
pub(crate) const AGENT: &str = concat!("Sonora/", env!("CARGO_PKG_VERSION"));

/// How long a connect may take. The other providers get ten seconds, but the addon
/// ecosystem runs on free tiers with cold starts — the reference client measured a worst case
/// of thirty-two seconds before an answer — so the ceiling here is wider: a link that only
/// needs waking must not read as a dead one.
const CONNECT: Duration = Duration::from_secs(30);
/// The spec asks addons to answer search in under five seconds; this leaves room for a cold
/// backend without holding a resolution slot for long.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(60);
/// Wider than search, because a TorBox-backed addon cold-caching a long FLAC spends twenty to
/// thirty seconds before it can answer at all.
const STREAM_TIMEOUT: Duration = Duration::from_secs(90);
/// How long a manifest is trusted. The spec's update story is "just update your server", so a
/// settings schema change is picked up within one sitting without a fetch per request.
const MANIFEST_TTL: Duration = Duration::from_secs(10 * 60);

/// What this provider asks an addon for. The names travel to the addon as-is when its manifest
/// does not enumerate a `quality` setting of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    HiRes,
    Lossless,
    High,
    Low,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HiRes => "HI_RES",
            Self::Lossless => "LOSSLESS",
            Self::High => "HIGH",
            Self::Low => "LOW",
        }
    }
}

/// One addon server.
pub struct AddonClient {
    http: reqwest::Client,
    base: String,
    manifest: Mutex<Option<(Instant, Manifest)>>,
}

impl AddonClient {
    /// Builds a client for `raw`, a URL the listener pasted: with or without `/manifest.json`,
    /// with or without a trailing slash. A token in the path is left intact, since every later
    /// call has to carry it.
    pub fn new(raw: &str) -> Result<Self> {
        let base = normalize_base(raw);
        Url::parse(&base).context("that is not a usable addon address")?;
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT)
            .build()
            .context("cannot build the addon http client")?;
        Ok(Self {
            http,
            base,
            manifest: Mutex::new(None),
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// The http client, so playback downloads a resolved stream over the same pool and
    /// connect timeout every addon call uses.
    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// What the addon says it is. Cached for the manifest TTL; a failure is not cached, so a
    /// server having a bad minute is asked again on the next call.
    pub async fn manifest(&self) -> Result<Manifest> {
        {
            let cached = self.manifest.lock().unwrap_or_else(|p| p.into_inner());
            if let Some((at, manifest)) = cached.as_ref()
                && at.elapsed() < MANIFEST_TTL
            {
                return Ok(manifest.clone());
            }
        }
        let manifest: Manifest = self
            .fetch(self.url(&["manifest.json"], &[])?, SEARCH_TIMEOUT)
            .await
            .context("cannot read the addon manifest")?;
        if manifest.id.trim().is_empty() {
            bail!("that url answered, but not with an addon manifest");
        }
        if !manifest.is_playable() {
            let declared = manifest.resources.join(", ");
            bail!("this addon declares {declared} - an addon must declare search");
        }
        let mut cached = self.manifest.lock().unwrap_or_else(|p| p.into_inner());
        *cached = Some((Instant::now(), manifest.clone()));
        Ok(manifest)
    }

    /// Tracks the addon holds for `query`.
    pub async fn search(&self, query: &str, tier: Tier) -> Result<Vec<AddonTrack>> {
        self.search_within(query, tier, SEARCH_TIMEOUT).await
    }

    /// The same search, with a shorter patience for a caller that already has something else
    /// playing and would rather walk past than wait.
    pub async fn search_within(
        &self,
        query: &str,
        tier: Tier,
        timeout: Duration,
    ) -> Result<Vec<AddonTrack>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let params = self.settings_for(tier).await;
        let url = self.url(&["search"], &params)?;
        let url = append_pair(url, "q", trimmed);
        let answer: SearchResponse = self
            .fetch(url, timeout)
            .await
            .context("the addon search failed")?;
        Ok(answer.tracks)
    }

    /// A playable URL for one of this addon's own track ids. `Ok(None)` is the addon saying it
    /// does not hold that recording — a miss, not a fault.
    pub async fn stream(&self, track_id: &str, tier: Tier) -> Result<Option<AddonStream>> {
        self.stream_within(track_id, tier, STREAM_TIMEOUT).await
    }

    /// The same resolution, with a shorter patience for a caller that already has something
    /// else playing.
    pub async fn stream_within(
        &self,
        track_id: &str,
        tier: Tier,
        timeout: Duration,
    ) -> Result<Option<AddonStream>> {
        let params = self.settings_for(tier).await;
        let url = self.url(&["stream", track_id], &params)?;
        let text = match self.get(url, timeout).await? {
            Some(text) => text,
            None => return Ok(None),
        };
        let stream: AddonStream =
            serde_json::from_str(&text).context("the addon stream answer did not parse")?;
        if stream.url.trim().is_empty() {
            if let Some(reason) = stream.error.as_deref() {
                log::warn!(
                    "addons: {} has no stream for {track_id}: {reason}",
                    redact(&self.base)
                );
            }
            return Ok(None);
        }
        Ok(Some(stream))
    }

    /// The query parameters that travel with every request: the addon's declared defaults
    /// first, then `quality`, negotiated against the options the manifest enumerates so an
    /// addon expecting `high|normal|low` is not sent `LOSSLESS` — an invalid value that lands
    /// on its fallback. The manifest is read best-effort: an addon whose manifest is briefly
    /// unreachable while its endpoints work is still asked, one tier hint the poorer.
    async fn settings_for(&self, tier: Tier) -> Vec<(String, String)> {
        let manifest = self.manifest().await.ok();
        let mut params: Vec<(String, String)> = Vec::new();
        if let Some(manifest) = manifest.as_ref() {
            for setting in &manifest.settings {
                let key = setting.key.trim();
                if key.is_empty() {
                    continue;
                }
                if let Some(value) = setting.default_value() {
                    params.push((key.to_owned(), value));
                }
            }
            let options: Vec<String> = manifest
                .settings
                .iter()
                .find(|setting| setting.key == "quality")
                .map(|setting| {
                    setting
                        .options
                        .iter()
                        .filter_map(|o| o.string_value())
                        .collect()
                })
                .unwrap_or_default();
            if let Some(matched) = match_tier(tier, &options) {
                params.retain(|(key, _)| key != "quality");
                params.push(("quality".into(), matched));
            } else {
                params.retain(|(key, _)| key != "quality");
                params.push(("quality".into(), tier.as_str().into()));
            }
        } else {
            params.push(("quality".into(), tier.as_str().into()));
        }
        params
    }

    /// One GET with the fields every endpoint shares. `Ok(None)` is a 404, which on `/stream`
    /// is the protocol's way of saying the addon does not hold that recording.
    async fn get(&self, url: Url, timeout: Duration) -> Result<Option<String>> {
        let response = self
            .http
            .get(url.clone())
            .timeout(timeout)
            .header("Accept", "application/json")
            .header("User-Agent", AGENT)
            .send()
            .await
            .with_context(|| format!("cannot reach {}", redact(url.as_str())))?;
        let status = response.status().as_u16();
        if status == 404 {
            return Ok(None);
        }
        if !response.status().is_success() {
            bail!("{} answered HTTP {status}", redact(url.as_str()));
        }
        let text = response
            .text()
            .await
            .context("cannot read the addon answer")?;
        if text.trim().is_empty() {
            bail!("{} answered an empty body", redact(url.as_str()));
        }
        Ok(Some(text))
    }

    async fn fetch<T: serde::de::DeserializeOwned>(
        &self,
        url: Url,
        timeout: Duration,
    ) -> Result<T> {
        let Some(text) = self.get(url, timeout).await? else {
            bail!("the addon answered 404");
        };
        serde_json::from_str(&text).context("the addon answer did not parse")
    }

    /// `{base}/{segments…}?{params}`, with every part properly encoded: an addon's track ids
    /// are whatever its backend uses, and one holding a `/` or `?` must not rewrite the
    /// request into a different endpoint.
    fn url(&self, segments: &[&str], params: &[(String, String)]) -> Result<Url> {
        let mut url = Url::parse(&self.base).context("that is not a usable addon address")?;
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| anyhow::anyhow!("that address cannot carry a path"))?;
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        if !params.is_empty() {
            let mut query = url.query_pairs_mut();
            for (key, value) in params {
                query.append_pair(key, value);
            }
        }
        Ok(url)
    }
}

/// A base URL from whatever the listener pasted: `https://host/`, `https://host/manifest.json`
/// and every `.json` filename, since the box addons come from identifies the document itself.
pub fn normalize_base(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    let last_segment = trimmed.rsplit('/').next().unwrap_or_default();
    if last_segment.to_lowercase().ends_with(".json") && trimmed.contains("://") {
        trimmed[..trimmed.len() - last_segment.len()]
            .trim_end_matches('/')
            .to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// An addon URL with its path hidden, for logs: the protocol puts a listener's private token
/// in the path, so a line carrying the path carries the credential.
pub fn redact(url: &str) -> String {
    match Url::parse(url) {
        Ok(parsed) => format!(
            "{}://{}/***",
            parsed.scheme(),
            parsed.host_str().unwrap_or_default()
        ),
        Err(_) => "***".to_owned(),
    }
}

/// The option in `options` that best answers a request for `tier`, or `None` when the addon
/// enumerated nothing. Matched on what an option *says*, so an addon listing its tiers
/// best-first and one listing them worst-first are both read correctly.
fn match_tier(tier: Tier, options: &[String]) -> Option<String> {
    if options.is_empty() {
        return None;
    }
    let tier_name = tier.as_str();
    if let Some(exact) = options
        .iter()
        .find(|option| option.eq_ignore_ascii_case(tier_name))
    {
        return Some(exact.clone());
    }
    let wanted: &[&str] = match tier {
        Tier::HiRes => &[
            "hi-res", "hires", "hi_res", "lossless", "flac", "max", "best",
        ],
        Tier::Lossless => &["lossless", "flac", "hifi", "hi-res", "hires", "max", "best"],
        Tier::High => &["high", "320", "normal", "standard"],
        Tier::Low => &["low", "96", "128", "min"],
    };
    options
        .iter()
        .find(|option| {
            let lower = option.to_lowercase();
            wanted.iter().any(|word| lower.contains(word))
        })
        .cloned()
        .or_else(|| options.first().cloned())
}

/// Appends one more query parameter to a built URL.
fn append_pair(mut url: Url, key: &str, value: &str) -> Url {
    url.query_pairs_mut().append_pair(key, value);
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_normalization_takes_every_paste_shape() {
        assert_eq!(normalize_base("https://host/"), "https://host");
        assert_eq!(normalize_base("https://host/manifest.json"), "https://host");
        assert_eq!(normalize_base("https://host/addon.json/"), "https://host");
        assert_eq!(
            normalize_base("https://host/tok/manifest.json"),
            "https://host/tok"
        );
        assert_eq!(normalize_base(" https://host "), "https://host");
    }

    #[test]
    fn tier_matching_prefers_what_the_addon_says() {
        let options = ["HI_RES", "LOSSLESS", "HIGH", "LOW"].map(str::to_owned);
        assert_eq!(
            match_tier(Tier::Lossless, &options).as_deref(),
            Some("LOSSLESS")
        );
        assert_eq!(match_tier(Tier::High, &options).as_deref(), Some("HIGH"));
        let words = ["high", "normal", "low"].map(str::to_owned);
        assert_eq!(match_tier(Tier::Lossless, &words).as_deref(), Some("high"));
        assert_eq!(match_tier(Tier::Low, &words).as_deref(), Some("low"));
        assert_eq!(match_tier(Tier::High, &[]), None);
    }

    #[test]
    fn redaction_keeps_the_host_and_hides_the_path() {
        assert_eq!(
            redact("https://host/secret-token/stream"),
            "https://host/***"
        );
        assert_eq!(redact("not a url"), "***");
    }
}
