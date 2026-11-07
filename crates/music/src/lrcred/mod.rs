//! lrc.red: Apple Music's own lyric documents, per syllable, filed by ISRC.
//!
//! Every document sits at `https://lrc.red/s/{ISRC}.ttml`, so a recording that is already known
//! costs one request and cannot come back as another edit. Without an ISRC the site's own search
//! is asked with the title and the artist, and its hits are matched here rather than trusted in
//! order: it is a free-text search, and it happily ranks a remix or a live cut above the song.
//!
//! A miss is recoverable, since the other providers get their turn, but the wrong words scrolling
//! in time with the right song is not — so every test below is a requirement and not a score.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use std::sync::LazyLock;

use crate::lyrics::ttml;
use crate::{LyricsHit, LyricsProvider, LyricsQuery};

const SOURCE: &str = "lrc.red";
/// Ahead of the other Apple hosts: lrc.red is where they get their documents, so asking it
/// directly is the same timing without the middleman.
const TRUST: u32 = 190;
const ENDPOINT: &str = "https://lrc.red/search.json";
const DOCUMENTS: &str = "https://lrc.red/s/";
const AGENT: &str = concat!(
    "sonora/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/sonorahq/sonora)"
);
/// How far a hit's length may sit from the playing track's. YouTube reports whole seconds, and
/// remasters differ from their originals by a second or two, so this only rules out a wrong cut
/// rather than choosing between two right ones.
const TOLERANCE: f64 = 3.0;

pub struct LrcRed {
    http: reqwest::Client,
}

impl LrcRed {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
    }

    /// The hits the site lists for one question, best first.
    async fn find(&self, question: &str) -> Result<Vec<Hit>> {
        let response = self
            .http
            .get(ENDPOINT)
            .query(&[("q", question.trim())])
            .header("User-Agent", AGENT)
            .send()
            .await
            .context("cannot reach lrc.red")?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        if !status.is_success() {
            anyhow::bail!("lrc.red answered with status {status}");
        }
        let found: Answer = response
            .json()
            .await
            .context("cannot read the lrc.red response")?;
        Ok(found.hits.unwrap_or_default())
    }

    async fn document(&self, isrc: &str) -> Result<String> {
        let response = self
            .http
            .get(format!("{DOCUMENTS}{isrc}.ttml"))
            .header("User-Agent", AGENT)
            .send()
            .await
            .context("cannot reach the lrc.red document")?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("the lrc.red document answered with status {status}");
        }
        response
            .text()
            .await
            .context("cannot read the lrc.red document")
    }
}

impl Default for LrcRed {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LyricsProvider for LrcRed {
    fn name(&self) -> &'static str {
        SOURCE
    }

    async fn search(&self, query: &LyricsQuery) -> Result<Vec<LyricsHit>> {
        let mut asked = HashSet::new();
        for question in questions(query) {
            if !asked.insert(question.clone()) {
                continue;
            }
            let hits = self.find(&question).await?;
            let Some(best) = best(&hits, query) else {
                continue;
            };
            let Some(isrc) = best.isrc.as_deref() else {
                continue;
            };
            let sheet = ttml::parse(&self.document(isrc).await?)?;
            if sheet.lyrics.is_empty() {
                continue;
            }

            return Ok(vec![LyricsHit {
                source: SOURCE,
                trust: TRUST,
                lyrics: sheet.lyrics,
                instrumental: false,
                title: best.title.clone().unwrap_or_default(),
                artist: best.artist.clone().unwrap_or_default(),
                album: best.album.clone().filter(|album| !album.is_empty()),
                duration: best.duration.map(Duration::from_secs_f64),
                writers: sheet.writers,
            }]);
        }
        Ok(Vec::new())
    }
}

/// The questions to put to the search, most specific first: the whole credit, then the first
/// artist alone, then the title by itself. lrc.red answers a title on its own well and a crowded
/// question not at all, and a looser question is no licence to trust a hit: every one goes
/// through the same matching, which still wants the artist, the edit and the length.
fn questions(query: &LyricsQuery) -> Vec<String> {
    let mut questions = vec![format!("{} {}", query.title, query.artist)];
    if let Some(primary) = primary_artist(&query.artist) {
        questions.push(format!("{} {primary}", query.title));
    }
    questions.push(query.title.clone());
    questions
}

/// The first credit on a track, which is usually the artist the catalogue files the recording
/// under.
fn primary_artist(artist: &str) -> Option<&str> {
    let first = SEPARATORS.split(artist).next()?.trim();
    (!first.is_empty() && first != artist).then_some(first)
}

/// The hit that is this recording, or none if none of them is sure to be.
fn best<'a>(hits: &'a [Hit], query: &LyricsQuery) -> Option<&'a Hit> {
    let wanted_title = core(&query.title);
    let wanted_version = version(&query.title);
    let wanted_artists = artists(&query.artist);
    let length = query.duration.as_secs_f64();

    let distance = |hit: &Hit| match (length > 0.0, hit.duration) {
        (true, Some(seconds)) => (seconds - length).abs(),
        _ => 0.0,
    };

    hits.iter()
        .filter(|hit| {
            let Some(name) = hit.title.as_deref() else {
                return false;
            };
            hit.isrc
                .as_deref()
                .is_some_and(|isrc| !isrc.trim().is_empty())
                && core(name) == wanted_title
                && version(name) == wanted_version
                && (wanted_artists.is_empty()
                    || artists(hit.artist.as_deref().unwrap_or_default())
                        .iter()
                        .any(|artist| wanted_artists.contains(artist)))
                && distance(hit) <= TOLERANCE
        })
        .min_by(|left, right| distance(left).total_cmp(&distance(right)))
}

/// "Song (Live) - 2011 Remaster" → "song".
fn core(title: &str) -> String {
    let bare = BRACKETED.replace_all(title, " ");
    let bare = bare.split(" - ").next().unwrap_or(&bare);
    match normalized(bare).is_empty() {
        true => normalized(title),
        false => normalized(bare),
    }
}

/// The version words in the parts [`core`] throws away: a live cut and a remaster are not the
/// same recording, but a remaster and its original are.
fn version(title: &str) -> HashSet<String> {
    let mut extras: String = BRACKETED
        .find_iter(title)
        .map(|found| found.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    extras.push(' ');
    extras.push_str(title.split(" - ").nth(1).unwrap_or_default());
    normalized(&extras)
        .split(' ')
        .filter(|word| VERSION_WORDS.contains(word))
        .map(str::to_owned)
        .collect()
}

fn artists(artist: &str) -> HashSet<String> {
    SEPARATORS
        .split(artist)
        .map(normalized)
        .filter(|name| !name.is_empty())
        .collect()
}

/// Lowercased, accents off ("ROSÉ" is "rose"), and only letters and digits.
fn normalized(value: &str) -> String {
    let lowered = value.to_lowercase();
    let decomposed: String = lowered
        .chars()
        .filter(|character| !is_combining(*character))
        .map(|character| match character.is_alphanumeric() {
            true => character,
            false => ' ',
        })
        .collect();
    decomposed.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether a character is a combining mark, which a decomposed accent leaves behind.
fn is_combining(character: char) -> bool {
    matches!(character as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x20D0..=0x20FF)
}

static BRACKETED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[(\[][^)\]]*[)\]]").expect("a valid pattern"));

static SEPARATORS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*(?:,|&|;|/|\s+and\s+|\s+x\s+|\s+with\s+|\s+feat\.?\s+|\s+ft\.?\s+)\s*")
        .expect("a valid pattern")
});

/// Bracket words that name a different recording rather than a different label.
static VERSION_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "live",
        "remix",
        "remixed",
        "mix",
        "acoustic",
        "unplugged",
        "instrumental",
        "karaoke",
        "cappella",
        "acapella",
        "demo",
        "edit",
        "version",
        "cover",
        "sped",
        "slowed",
        "reverb",
        "nightcore",
        "lofi",
        "orchestral",
        "extended",
    ]
    .into_iter()
    .collect()
});

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    hits: Option<Vec<Hit>>,
}

#[derive(Deserialize)]
struct Hit {
    #[serde(default)]
    isrc: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    artist: Option<String>,
    #[serde(default)]
    album: Option<String>,
    /// Seconds, fractional.
    #[serde(default)]
    duration: Option<f64>,
}
