//! BetterLyrics' two catalogues: Apple's word timings and QQ Music's karaoke lines.

use anyhow::{Context as _, Result};
use async_trait::async_trait;

use crate::lyrics::{catalog, document};
use crate::{LyricsHit, LyricsProvider, LyricsQuery};

const APPLE: &str = "https://lyrics-api.boidu.dev/getLyrics";
const PORTATO: &str = "https://lyrics-api.boidu.dev/qq/getLyrics";
const AGENT: &str = concat!(
    "sonora/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/sonorahq/sonora)"
);

#[derive(Clone, Copy)]
pub enum Kind {
    Apple,
    Portato,
}

pub struct BetterLyrics {
    kind: Kind,
    http: reqwest::Client,
}

impl BetterLyrics {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            http: reqwest::Client::new(),
        }
    }

    pub fn apple() -> Self {
        Self::new(Kind::Apple)
    }

    pub fn portato() -> Self {
        Self::new(Kind::Portato)
    }

    fn endpoint(&self) -> &'static str {
        match self.kind {
            Kind::Apple => APPLE,
            Kind::Portato => PORTATO,
        }
    }

    fn source(&self) -> &'static str {
        match self.kind {
            Kind::Apple => "BetterLyrics",
            Kind::Portato => "BetterLyrics Portato",
        }
    }

    async fn body(&self, query: &LyricsQuery) -> Result<String> {
        let mut params = vec![("s", query.title.clone()), ("a", query.artist.clone())];
        if !query.duration.is_zero() {
            params.push(("d", query.duration.as_secs().to_string()));
        }
        if let Some(album) = query.album.as_deref().filter(|album| !album.is_empty()) {
            params.push(("al", album.to_owned()));
        }

        let response = self
            .http
            .get(self.endpoint())
            .query(&params)
            .header("User-Agent", AGENT)
            .send()
            .await
            .context("cannot reach betterlyrics")?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("betterlyrics answered with status {status}");
        }
        response
            .text()
            .await
            .context("cannot read the betterlyrics response")
    }
}

#[async_trait]
impl LyricsProvider for BetterLyrics {
    fn name(&self) -> &'static str {
        self.source()
    }

    async fn search(&self, query: &LyricsQuery) -> Result<Vec<LyricsHit>> {
        let body = self.body(query).await?;
        let Some(lyrics) = document::read(&body) else {
            return Ok(Vec::new());
        };
        let trust = match self.kind {
            Kind::Apple => 160,
            Kind::Portato => 150,
        };
        Ok(vec![catalog::hit_with_trust(
            self.source(),
            query,
            lyrics,
            trust,
        )])
    }
}
