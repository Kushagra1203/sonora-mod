//! PaxSenix's lyric endpoints: Apple's catalogue, Musixmatch and Spotify.
//!
//! The Apple endpoint is keyless and takes an Apple song id, so it is found through the public
//! iTunes search first. The other two authenticate with `PAXSENIX_API_KEY`; without it they
//! answer nothing and the remaining sources carry the lookup.

use std::time::Duration;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use serde::Deserialize;

use crate::lyrics::{catalog, document, lrc};
use crate::{Lyrics, LyricsHit, LyricsLine, LyricsProvider, LyricsQuery, Voice};

const API: &str = "https://api.paxsenix.org";
const APPLE: &str = "https://lyrics.paxsenix.org/apple-music/lyrics";
const ITUNES: &str = "https://itunes.apple.com/search";
const AGENT: &str = concat!(
    "sonora/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/sonorahq/sonora)"
);

#[derive(Clone, Copy)]
pub enum Kind {
    Apple,
    Musixmatch,
    Spotify,
}

pub struct PaxSenix {
    kind: Kind,
    key: Option<String>,
    http: reqwest::Client,
}

impl PaxSenix {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            key: key(),
            http: reqwest::Client::new(),
        }
    }

    pub fn create_apple() -> Self {
        Self::new(Kind::Apple)
    }

    pub fn create_musixmatch() -> Self {
        Self::new(Kind::Musixmatch)
    }

    pub fn create_spotify() -> Self {
        Self::new(Kind::Spotify)
    }

    fn source(&self) -> &'static str {
        match self.kind {
            Kind::Apple => "PaxSenix",
            Kind::Musixmatch => "PaxSenix: Musixmatch",
            Kind::Spotify => "PaxSenix: Spotify",
        }
    }

    fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    async fn itunes(&self, query: &LyricsQuery) -> Result<Option<Found>> {
        let response = self
            .http
            .get(ITUNES)
            .query(&[
                ("term", format!("{} {}", query.title, query.artist)),
                ("entity", "song".to_owned()),
                ("limit", "10".to_owned()),
            ])
            .header("User-Agent", AGENT)
            .send()
            .await
            .context("cannot reach the iTunes catalogue")?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("the iTunes catalogue answered with status {status}");
        }
        let found: Itunes = response
            .json()
            .await
            .context("cannot read the iTunes catalogue response")?;
        Ok(found
            .results
            .into_iter()
            .filter_map(|track| {
                let candidate = track.hit(0);
                crate::lyrics::matched(query, &candidate)
                    .then(|| (crate::lyrics::score(query, &candidate), track))
            })
            .max_by_key(|(score, _)| *score)
            .map(|(_, track)| track))
    }

    async fn search_apple(&self, query: &LyricsQuery) -> Result<Vec<LyricsHit>> {
        let Some(found) = self.itunes(query).await? else {
            return Ok(Vec::new());
        };
        let response = self
            .http
            .get(APPLE)
            .query(&[("id", found.id.to_string()), ("ttml", "true".to_owned())])
            .header("User-Agent", AGENT)
            .send()
            .await
            .context("cannot reach PaxSenix's Apple lyrics")?;
        let status = response.status();
        if !status.is_success() {
            log::warn!("lyrics: PaxSenix Apple answered with status {status}");
            return Ok(Vec::new());
        }
        let body = response
            .text()
            .await
            .context("cannot read PaxSenix's Apple lyrics")?;
        let Some(lyrics) = document::read(&body) else {
            return Ok(Vec::new());
        };

        Ok(vec![LyricsHit {
            source: self.source(),
            trust: 170,
            lyrics,
            instrumental: false,
            title: found.name,
            artist: found.artist,
            album: found.album.filter(|album| !album.is_empty()),
            duration: found.duration.map(Duration::from_millis),
            writers: Vec::new(),
        }])
    }

    async fn search_musixmatch(&self, query: &LyricsQuery) -> Result<Vec<LyricsHit>> {
        let Some(key) = self.key() else {
            return Ok(Vec::new());
        };
        let mut params = vec![("t", query.title.clone()), ("a", query.artist.clone())];
        if !query.duration.is_zero() {
            params.push(("d", query.duration.as_secs().to_string()));
        }
        let body = self.api("lyrics/musixmatch", &params, key).await?;
        let Some(body) = body else {
            return Ok(Vec::new());
        };
        let Some(lyrics) = musixmatch_lyrics(&body).or_else(|| document::read(&body)) else {
            return Ok(Vec::new());
        };
        Ok(vec![catalog::hit_with_trust(
            self.source(),
            query,
            lyrics,
            80,
        )])
    }

    async fn search_spotify(&self, query: &LyricsQuery) -> Result<Vec<LyricsHit>> {
        let Some(key) = self.key() else {
            return Ok(Vec::new());
        };
        let found = self.spotify_track(query, key).await?;
        let lyrics = match found.as_ref().and_then(|track| track.id.as_deref()) {
            Some(id) => {
                let body = self
                    .api("lyrics/spotify", &[("id", id.to_owned())], key)
                    .await?;
                body.and_then(|body| spotify_lyrics(&body).or_else(|| document::read(&body)))
            }
            None => None,
        };
        // Spotify's endpoint misses some tracks; the generic keyless-by-id search still answers.
        let lyrics = match lyrics {
            Some(lyrics) => Some(lyrics),
            None => {
                let body = self
                    .api(
                        "lyrics/lrcget",
                        &[("q", format!("{} {}", query.title, query.artist))],
                        key,
                    )
                    .await?;
                body.and_then(|body| document::read(&body))
            }
        };
        let Some(lyrics) = lyrics else {
            return Ok(Vec::new());
        };
        let hit = match found {
            Some(track) => track.hit(70),
            None => catalog::hit_with_trust(self.source(), query, lyrics.clone(), 70),
        };
        Ok(vec![LyricsHit { lyrics, ..hit }])
    }

    async fn spotify_track(&self, query: &LyricsQuery, key: &str) -> Result<Option<SpotifyTrack>> {
        let body = self
            .api(
                "spotify/search",
                &[("q", format!("{} {}", query.title, query.artist))],
                key,
            )
            .await?;
        let Some(body) = body else {
            return Ok(None);
        };
        let found: SpotifySearch = serde_json::from_str(&body).unwrap_or_default();
        Ok(found
            .tracks
            .items
            .into_iter()
            .filter_map(|track| {
                let candidate = track.hit(0);
                crate::lyrics::matched(query, &candidate)
                    .then(|| (crate::lyrics::score(query, &candidate), track))
            })
            .max_by_key(|(score, _)| *score)
            .map(|(_, track)| track))
    }

    async fn api(
        &self,
        path: &str,
        params: &[(&str, String)],
        key: &str,
    ) -> Result<Option<String>> {
        let response = self
            .http
            .get(format!("{API}/{path}"))
            .query(params)
            .header(
                "Authorization",
                format!("Bearer {}", key.trim_start_matches("Bearer ").trim()),
            )
            .header("User-Agent", AGENT)
            .send()
            .await
            .with_context(|| format!("cannot reach PaxSenix's {path}"))?;
        let status = response.status();
        if !status.is_success() {
            log::warn!("lyrics: PaxSenix {path} answered with status {status}");
            return Ok(None);
        }
        Ok(Some(response.text().await.with_context(|| {
            format!("cannot read PaxSenix's {path}")
        })?))
    }
}

fn key() -> Option<String> {
    std::env::var("PAXSENIX_API_KEY")
        .ok()
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
}

#[async_trait]
impl LyricsProvider for PaxSenix {
    fn name(&self) -> &'static str {
        self.source()
    }

    async fn search(&self, query: &LyricsQuery) -> Result<Vec<LyricsHit>> {
        match self.kind {
            Kind::Apple => self.search_apple(query).await,
            Kind::Musixmatch => self.search_musixmatch(query).await,
            Kind::Spotify => self.search_spotify(query).await,
        }
    }
}

#[derive(Deserialize, Default)]
struct Itunes {
    #[serde(default)]
    results: Vec<Found>,
}

#[derive(Deserialize, Default)]
struct Found {
    #[serde(default, rename = "trackId")]
    id: u64,
    #[serde(default, rename = "trackName")]
    name: String,
    #[serde(default, rename = "artistName")]
    artist: String,
    #[serde(default, rename = "collectionName")]
    album: Option<String>,
    #[serde(default, rename = "trackTimeMillis")]
    duration: Option<u64>,
}

impl Found {
    fn hit(&self, trust: u32) -> LyricsHit {
        LyricsHit {
            source: "PaxSenix",
            trust,
            lyrics: Lyrics::plain(String::new()),
            instrumental: false,
            title: self.name.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
            duration: self.duration.map(Duration::from_millis),
            writers: Vec::new(),
        }
    }
}

#[derive(Deserialize, Default)]
struct SpotifySearch {
    #[serde(default)]
    tracks: SpotifyTracks,
}

#[derive(Deserialize, Default)]
struct SpotifyTracks {
    #[serde(default)]
    items: Vec<SpotifyTrack>,
}

#[derive(Deserialize, Default)]
struct SpotifyTrack {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default, rename = "duration_ms")]
    duration: Option<u64>,
    #[serde(default)]
    artists: Vec<SpotifyArtist>,
}

#[derive(Deserialize, Default)]
struct SpotifyArtist {
    #[serde(default)]
    name: String,
}

impl SpotifyTrack {
    fn hit(&self, trust: u32) -> LyricsHit {
        LyricsHit {
            source: "PaxSenix: Spotify",
            trust,
            lyrics: Lyrics::plain(String::new()),
            instrumental: false,
            title: self.name.clone(),
            artist: self
                .artists
                .iter()
                .map(|artist| artist.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            album: None,
            duration: self.duration.map(Duration::from_millis),
            writers: Vec::new(),
        }
    }
}

#[derive(Deserialize)]
struct MmAnswer {
    #[serde(default)]
    lyrics: Vec<MmLine>,
    #[serde(default)]
    lrc: Option<String>,
}

#[derive(Deserialize)]
struct MmLine {
    #[serde(default)]
    text: String,
    #[serde(default)]
    time: Option<MmTime>,
}

#[derive(Deserialize)]
struct MmTime {
    #[serde(default)]
    total: f64,
}

fn musixmatch_lyrics(body: &str) -> Option<Lyrics> {
    let answer: MmAnswer = serde_json::from_str(body).ok()?;
    if let Some(lrc) = answer.lrc.as_deref().filter(|lrc| !lrc.trim().is_empty()) {
        let lines = lrc::parse(lrc);
        if !lines.is_empty() {
            return Some(Lyrics::Synced {
                lines: lines.into(),
            });
        }
    }
    let lines: Vec<LyricsLine> = answer
        .lyrics
        .into_iter()
        .filter(|line| !line.text.trim().is_empty())
        .filter_map(|line| {
            let at = line.time?.total;
            let start = Duration::try_from_secs_f64(at).ok()?;
            Some(LyricsLine {
                start,
                end: None,
                text: line.text,
                romanized: None,
                words: None,
                secondary: Vec::new(),
                voice: Voice::default(),
            })
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    let mut lines = lines;
    lrc::normalize(&mut lines);
    Some(Lyrics::Synced {
        lines: lines.into(),
    })
}

#[derive(Deserialize)]
struct SpAnswer {
    #[serde(default)]
    lyrics: Vec<SpLine>,
}

#[derive(Deserialize)]
struct SpLine {
    #[serde(default, rename = "timeTag")]
    time_tag: String,
    #[serde(default)]
    words: String,
    #[serde(default)]
    text: String,
}

fn spotify_lyrics(body: &str) -> Option<Lyrics> {
    let answer: SpAnswer = serde_json::from_str(body).ok()?;
    let lines: Vec<LyricsLine> = answer
        .lyrics
        .into_iter()
        .filter_map(|line| {
            let text = match line.words.is_empty() {
                true => line.text,
                false => line.words,
            };
            let start = stamp(&line.time_tag)?;
            (!text.trim().is_empty()).then(|| LyricsLine {
                start,
                end: None,
                text,
                romanized: None,
                words: None,
                secondary: Vec::new(),
                voice: Voice::default(),
            })
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    let mut lines = lines;
    lrc::normalize(&mut lines);
    Some(Lyrics::Synced {
        lines: lines.into(),
    })
}

/// Reads `mm:ss.xx` or `mm:ss:xx` into a position.
fn stamp(value: &str) -> Option<Duration> {
    let mut parts = value.trim().split([':', '.']);
    let minutes: u64 = parts.next()?.trim().parse().ok()?;
    let seconds: u64 = parts.next()?.trim().parse().ok()?;
    let fraction = parts.next().unwrap_or("0").trim();
    let hundredths: u64 = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u64>().ok()? * 10,
        2 => fraction.parse().ok()?,
        _ => fraction[..2].parse().ok()?,
    };
    Some(Duration::from_millis(
        minutes * 60_000 + seconds * 1_000 + hundredths * 10,
    ))
}

#[cfg(test)]
mod tests {
    use super::{musixmatch_lyrics, spotify_lyrics, stamp};
    use std::time::Duration;

    #[test]
    fn reads_a_musixmatch_answer() {
        let body = r#"{"ok":true,"lrc":"[00:01.50]Hello\n[00:03.00]World","lyrics":[]}"#;
        let lyrics = musixmatch_lyrics(body).unwrap();
        assert!(lyrics.synced());
    }

    #[test]
    fn reads_a_spotify_answer() {
        let body = r#"{"lyrics":[{"timeTag":"00:01.50","words":"Hello"},{"timeTag":"00:03.00","words":"World"}]}"#;
        let lyrics = spotify_lyrics(body).unwrap();
        assert!(lyrics.synced());
    }

    #[test]
    fn reads_a_timestamp() {
        assert_eq!(stamp("01:02.34"), Some(Duration::from_millis(62_340)));
    }
}
