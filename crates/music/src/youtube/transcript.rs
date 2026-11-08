//! A video's own timed captions, read the way the web player reads them.
//!
//! YouTube Music's lyrics tab is plain text, and a song upload carries no captions at all —
//! every one of them answers with an empty caption list. A video upload does carry them: the
//! caption track the player would show, with a time on every line.
//!
//! Which words those are depends on the video. A lyric video's captions are the song's. An
//! official music video's captions are the whole film: the dialogue before the song, the song
//! itself, and the dialogue after it. Showing the film's words as lyrics would be worse than
//! showing none, so the captions are read against YouTube Music's own lyrics tab for the same
//! video, which holds the song's words and nothing else, and the lines that are not the song's
//! are left out.
//!
//! The route is the web player's own: the player response names the caption track, and the
//! track's own address carries the timings. YouTube Music's transcript endpoint is gone — it
//! answers every request with a precondition failure — so this is the way that still works.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use serde_json::Value;
use ytmusic::{Client, YtMusic, nav::Nav as _};

use crate::lyrics::catalog;
use crate::youtube::lyrics::YouTubeLyrics;
use crate::{Lyrics, LyricsHit, LyricsLine, LyricsProvider, LyricsQuery, Voice};

const SOURCE: &str = "YouTube captions";

/// The fewest lines a caption sheet may keep and still be the song's.
const ENOUGH: usize = 4;

/// The timed captions of the video a track plays.
pub struct YouTubeTranscript {
    api: YtMusic,
    /// The plain lyrics YouTube Music holds for the same video, which say what the song's
    /// words are.
    tab: YouTubeLyrics,
    http: reqwest::Client,
}

impl YouTubeTranscript {
    pub fn new() -> Self {
        // The player endpoint answers a signed-out client with a bare response and no caption
        // tracks, so the session is used: it asks about one video the listener is already
        // playing and records nothing in the account's history.
        let jar = crate::credentials::dir("youtube").join("cookies.json");
        Self {
            api: YtMusic::with_cookies("").persist_cookies(jar),
            tab: YouTubeLyrics::new(),
            http: reqwest::Client::new(),
        }
    }

    /// The timed lines a video's captions hold, if they carry the song's words.
    async fn captions(&self, id: &str) -> Result<Option<Lyrics>> {
        let answer = self
            .api
            .execute(
                "player",
                Client::Web,
                serde_json::json!({
                    "videoId": id,
                    "contentCheckOk": true,
                    "racyCheckOk": true,
                }),
            )
            .await
            .context("cannot ask the player about the video")?;
        let tracks: Vec<Value> = answer
            .items(&[
                "captions",
                "playerCaptionsTracklistRenderer",
                "captionTracks",
            ])
            .to_vec();
        let Some(track) = pick(&tracks) else {
            return Ok(None);
        };
        let Some(url) = track.str_at(&["baseUrl"]) else {
            return Ok(None);
        };

        // The song's own words, which say which of the caption lines are the song's.
        let song = match self.tab.sheet(id).await? {
            Some(Lyrics::Plain { text, .. }) => words(&text),
            Some(Lyrics::Synced { lines }) => words(
                &lines
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            None => return Ok(None),
        };
        if song.is_empty() {
            return Ok(None);
        }

        let body = self
            .http
            .get(format!("{url}&fmt=json3"))
            .send()
            .await
            .context("cannot reach the captions")?
            .text()
            .await
            .context("cannot read the captions")?;
        Ok(sheet(&body, &song))
    }
}

impl Default for YouTubeTranscript {
    fn default() -> Self {
        Self::new()
    }
}

/// The caption track to read.
///
/// The uploader's own comes first: it is the one the player shows by default, and the one a
/// lyric video carries the words in. A track the uploader did not write is YouTube's own
/// transcription of what it heard, which is what a cover upload has.
fn pick(tracks: &[Value]) -> Option<&Value> {
    tracks
        .iter()
        .find(|track| track.str_at(&["kind"]).is_none_or(|kind| kind != "asr"))
        .or_else(|| tracks.first())
}

/// The timed lines a caption document holds that carry the song's words.
///
/// A music video's captions hold the film's dialogue as well as the song, and the dialogue is
/// what the video is mostly made of before the song starts. Each line is weighed against the
/// song's own words: a line whose words are mostly the song's is the song's, and a line whose
/// words are mostly the film's is left out.
fn sheet(body: &str, song: &HashSet<String>) -> Option<Lyrics> {
    let parsed: Value = serde_json::from_str(body).ok()?;
    let mut lines: Vec<LyricsLine> = Vec::new();
    for event in parsed.items(&["events"]) {
        let Some(start) = event.at(&["tStartMs"]).and_then(|value| value.as_u64()) else {
            continue;
        };
        let text: String = event
            .items(&["segs"])
            .iter()
            .filter_map(|seg| seg.str_at(&["utf8"]))
            .collect();
        let text = text.trim().trim_matches('♪').trim().to_owned();
        if text.is_empty() || !carries(&text, song) {
            continue;
        }
        // A caption that is still on screen is repeated by the next event, so the same words
        // with the same time are only kept once.
        let start_at = Duration::from_millis(start);
        if lines
            .last()
            .is_some_and(|last| last.text == text && last.start == start_at)
        {
            continue;
        }
        let end = event
            .at(&["dDurationMs"])
            .and_then(|value| value.as_u64())
            .map(|span| start_at + Duration::from_millis(span));
        lines.push(LyricsLine {
            start: start_at,
            end,
            text,
            romanized: None,
            words: None,
            secondary: Vec::new(),
            voice: Voice::Lead,
        });
    }
    lines.sort_by_key(|line| line.start);
    match lines.len() < ENOUGH {
        true => None,
        false => Some(Lyrics::Synced {
            lines: lines.into(),
        }),
    }
}

/// Whether a caption line carries the song's words rather than the film's.
///
/// A line is the song's when most of its words are the song's own. A line too short to be
/// weighed either way is kept: there is nothing in it to be the film's.
fn carries(line: &str, song: &HashSet<String>) -> bool {
    let weighed: Vec<String> = words(line)
        .into_iter()
        .filter(|word| word.chars().count() >= 3)
        .collect();
    if weighed.is_empty() {
        return true;
    }
    let found = weighed.iter().filter(|word| song.contains(*word)).count();
    found * 2 > weighed.len()
}

/// The words of a text, folded to letters and numbers and lowercased, so the song's own words
/// and the caption's are the same words however each of them is written.
fn words(text: &str) -> HashSet<String> {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|letter| letter.is_alphanumeric())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

#[async_trait]
impl LyricsProvider for YouTubeTranscript {
    fn name(&self) -> &'static str {
        SOURCE
    }

    async fn search(&self, query: &LyricsQuery) -> Result<Vec<LyricsHit>> {
        // Only the video the track plays has captions: a song upload carries none at all, so
        // there is nothing to ask for.
        let Some(id) = query.id_for("youtube") else {
            return Ok(Vec::new());
        };
        let Some(sheet) = self.captions(id).await? else {
            return Ok(Vec::new());
        };
        Ok(vec![catalog::hit(SOURCE, query, sheet)])
    }
}
