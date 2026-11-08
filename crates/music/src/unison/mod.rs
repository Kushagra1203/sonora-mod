//! Unison: a community-submitted lyrics database, and the only source here whose words are
//! contributed rather than licensed.
//!
//! That is its character. Every other source is a window onto somebody's catalogue, so it carries
//! what that catalogue has; this one carries what people have uploaded, which means it sometimes
//! has a track nothing else does and is thin everywhere else. It answers a single entry rather
//! than a list, and the entry names its own song, artist and album, so it is matched like any
//! other hit and a wrong one is refused rather than shown.
//!
//! Three shapes come back and the entry says which: Apple-style TTML with word timings, ordinary
//! line-stamped LRC, or plain text with no timing at all.

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use serde::Deserialize;

use crate::lyrics::{lrc, ttml};
use crate::{Lyrics, LyricsHit, LyricsProvider, LyricsQuery};

const SOURCE: &str = "Unison";
/// Low: the words are submitted by hand, so they are the ones to fall back on rather than the
/// ones to prefer over a licensed catalogue.
const TRUST: u32 = 30;
const ENDPOINT: &str = "https://unison.boidu.dev/lyrics";
const AGENT: &str = concat!(
    "sonora/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/sonorahq/sonora)"
);

pub struct Unison {
    http: reqwest::Client,
}

impl Unison {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
    }

    /// The entry the site holds for this recording, or none.
    ///
    /// A miss is answered `404` with the same JSON envelope as a hit, so it is read as "no
    /// lyrics" rather than as a failure to reach the site.
    async fn ask(&self, query: &LyricsQuery) -> Result<Option<Entry>> {
        let mut params = vec![
            ("song", query.title.clone()),
            ("artist", query.artist.clone()),
        ];
        if let Some(album) = &query.album
            && !album.is_empty()
        {
            params.push(("album", album.clone()));
        }
        if !query.duration.is_zero() {
            params.push(("duration", query.duration.as_secs().to_string()));
        }

        let response = self
            .http
            .get(ENDPOINT)
            .query(&params)
            .header("User-Agent", AGENT)
            .send()
            .await
            .context("cannot reach Unison")?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            anyhow::bail!("Unison answered with status {status}");
        }
        let answer: Answer = response
            .json()
            .await
            .context("cannot read the Unison response")?;
        match answer.success {
            true => Ok(answer.data),
            false => Ok(None),
        }
    }
}

impl Default for Unison {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LyricsProvider for Unison {
    fn name(&self) -> &'static str {
        SOURCE
    }

    async fn search(&self, query: &LyricsQuery) -> Result<Vec<LyricsHit>> {
        let Some(entry) = self.ask(query).await? else {
            return Ok(Vec::new());
        };
        let Some(text) = entry
            .lyrics
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        else {
            return Ok(Vec::new());
        };

        let (lyrics, writers) = read(&entry, text);
        if lyrics.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![LyricsHit {
            source: SOURCE,
            trust: TRUST,
            lyrics,
            instrumental: false,
            title: entry.song.unwrap_or_else(|| query.title.clone()),
            artist: entry.artist.unwrap_or_else(|| query.artist.clone()),
            album: entry
                .album
                .filter(|album| !album.is_empty())
                .or_else(|| query.album.clone()),
            // The entry does not state a length; the search already used the playing track's,
            // so claiming it back would make the duration check say nothing.
            duration: None,
            writers,
        }])
    }
}

/// The words in whichever shape the entry says they are.
fn read(entry: &Entry, text: &str) -> (Lyrics, Vec<String>) {
    let format = entry.format.as_deref().unwrap_or_default();
    if format.eq_ignore_ascii_case("ttml") || text.contains("<tt ") || text.contains("<tt>") {
        if let Ok(sheet) = ttml::parse(text)
            && !sheet.lyrics.is_empty()
        {
            return (sheet.lyrics, sheet.writers);
        }
    }

    // A plain entry is prose by its own account; anything else is offered to the LRC reader,
    // which also reads the word-stamped form that is still filed as `lrc`.
    let plain = entry
        .sync_type
        .as_deref()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("plain"));
    if !plain {
        let lines = lrc::parse(text);
        if !lines.is_empty() {
            return (
                Lyrics::Synced {
                    lines: lines.into(),
                },
                Vec::new(),
            );
        }
    }

    (prose(text), Vec::new())
}

/// Words with no timing: one line each, which the panel reads as text to scroll by hand.
fn prose(text: &str) -> Lyrics {
    Lyrics::plain(
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

#[derive(Deserialize, Default)]
struct Answer {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    data: Option<Entry>,
}

/// One entry from the database.
#[derive(Deserialize, Default)]
struct Entry {
    #[serde(default)]
    song: Option<String>,
    #[serde(default)]
    artist: Option<String>,
    #[serde(default)]
    album: Option<String>,
    #[serde(default)]
    lyrics: Option<String>,
    /// `ttml` or `lrc`.
    #[serde(default)]
    format: Option<String>,
    /// `wordsync`, `linesync` or `plain`.
    #[serde(default, rename = "syncType")]
    sync_type: Option<String>,
}
