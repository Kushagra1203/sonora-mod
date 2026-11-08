//! Where a listing's videos turn into the songs behind them.
//!
//! YouTube Music does not promise that a row plays the song: many rows point at the music video,
//! whose soundtrack carries the video's dialogue and runs longer. So the song behind a video is
//! searched for, and the row is rewritten to it while keeping the listing's own title, credits,
//! album and artwork.
//!
//! The search is the fragile part. Google answers a burst of them with a 403 challenge, and a
//! challenge is not an answer: caching it as "no song" would pin the row to its video for good,
//! which is what happened when the resolution cache was filled during a run of refusals. So only
//! a search that completed is kept, found or not; a failed one is forgotten and asked again the
//! next time the row is seen. Requests are paced, and a run of failures stops the resolver for a
//! while rather than digging the hole deeper.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use regex::Regex;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use tokio::task::JoinSet;
use ytmusic::{Track, TrackKind, YtMusic};

/// How many searches may be in flight together. A burst of six is what Google challenges.
const AT_ONCE: usize = 2;
/// The least time between two search requests, so even the two in flight stay a few a second.
const GAP: Duration = Duration::from_millis(250);
/// How much longer than its own listing promised a video may run and still be playing the song's
/// audio. Past this it carries something the song does not.
const TOLERANCE: Duration = Duration::from_secs(2);
/// How many searches in a row may fail before the resolver stops asking for a while.
const PATIENCE: u32 = 4;
/// How long it stays quiet after that, long enough for a challenge to lapse.
const QUIET: Duration = Duration::from_secs(10 * 60);
/// The least time between two rewrites of the cache file, so a page is written once.
const WRITE_EVERY: Duration = Duration::from_secs(5);

/// What a search can say: a song, no song for certain, or nothing at all because the search
/// itself failed. Only the first two are worth remembering.
enum Answer {
    Song(Box<Track>),
    None,
    Failed,
}

/// What one search for a song came back with.
enum Searched {
    /// The song, when the search found it.
    Song(Box<Track>),
    /// The exact question answered and the song is not in it.
    Nothing,
    /// The exact question was refused and a simpler one answered about something else, so
    /// nothing was learned about the song.
    Refused,
}

/// The songs behind videos, remembered on disk so a video is searched for only once.
pub struct Resolver {
    api: Arc<YtMusic>,
    path: PathBuf,
    known: Mutex<HashMap<String, Option<Track>>>,
    permits: Semaphore,
    /// When the last search started, so the next one waits out [`GAP`].
    start: AsyncMutex<Instant>,
    failures: AtomicU32,
    quiet_until: Mutex<Option<Instant>>,
    written: Mutex<Instant>,
}

impl Resolver {
    /// A resolver over `path`, which keeps the same shape the library used before it: a map from
    /// a video's id to the song behind it, `null` when there is none.
    pub fn new(api: Arc<YtMusic>, path: PathBuf) -> Self {
        let known = fs::read(&path)
            .ok()
            .and_then(|body| serde_json::from_slice(&body).ok())
            .unwrap_or_default();
        Self {
            api,
            path,
            known: Mutex::new(known),
            permits: Semaphore::new(AT_ONCE),
            start: AsyncMutex::new(Instant::now() - GAP),
            failures: AtomicU32::new(0),
            quiet_until: Mutex::new(None),
            written: Mutex::new(Instant::now() - WRITE_EVERY),
        }
    }

    /// The list with every video replaced by the song behind it, in order. Rows already holding
    /// songs are left alone.
    pub async fn swap(self: &Arc<Self>, tracks: Vec<Track>) -> Vec<Track> {
        let mut out = tracks;
        let mut tasks = JoinSet::new();
        for (index, track) in out.iter().enumerate() {
            if !track.is_video() || track.video_id.is_none() {
                continue;
            }
            let this = self.clone();
            let track = track.clone();
            tasks.spawn(async move { (index, this.one(track).await) });
        }
        while let Some(result) = tasks.join_next().await {
            if let Ok((index, track)) = result {
                out[index] = track;
            }
        }
        out
    }

    /// The song behind one track, if there is one. Answers from the cache when it can, and
    /// leaves the track as it is when the search fails.
    pub async fn one(self: &Arc<Self>, track: Track) -> Track {
        if !track.is_video() {
            return track;
        }
        if track.video_id.is_none() {
            return track;
        }
        if let Some(known) = self.cached(&track) {
            return match known {
                Some(song) => applied(track, song),
                None => track,
            };
        }
        match self.ask(&track).await {
            Answer::Song(song) => {
                self.record(&track, Some((*song).clone()));
                applied(track, *song)
            }
            Answer::None => {
                self.record(&track, None);
                track
            }
            Answer::Failed => track,
        }
    }

    /// The id of the song behind a video, when it is known.
    ///
    /// A station seeded by a video answers with videos, and every one of them would then need a
    /// search of its own. A station seeded by the song answers with songs, so one lookup for the
    /// seed turns the whole queue into audio.
    pub async fn song_id(self: &Arc<Self>, video_id: &str) -> String {
        let Some(track) = self.track_of(video_id).await else {
            return video_id.to_owned();
        };
        let song = self.one(track).await;
        match song.is_video() {
            true => video_id.to_owned(),
            false => song.video_id.unwrap_or_else(|| video_id.to_owned()),
        }
    }

    /// The video as a row the resolver can look up, read from YouTube's own watch queue for it.
    /// The queue carries the title and the credit the video was published under, which is what
    /// the search needs.
    async fn track_of(&self, video_id: &str) -> Option<Track> {
        let _permit = self.turn().await;
        let answer = self
            .api
            .execute(
                "next",
                ytmusic::Client::Music,
                serde_json::json!({ "videoId": video_id }),
            )
            .await
            .ok()?;
        let track = ytmusic::parse::find_renderers(&answer, "playlistPanelVideoRenderer")
            .into_iter()
            .next()
            .and_then(ytmusic::parse::panel_track)?;
        Some(track)
    }

    /// What is known about the song behind `track`, `Some(None)` when it is known to have none.
    ///
    /// The same video can sit in two rows of a listing with different credits, and each wants
    /// its own song: the entry for one of them says nothing about the other. So the row's own
    /// title and credits are part of the key, and an entry under the bare video id is only
    /// trusted when the song it holds is the song this row would have found.
    fn cached(&self, track: &Track) -> Option<Option<Track>> {
        let known = self.known.lock().ok()?;
        if let Some(answer) = known.get(&key_of(track)) {
            return Some(answer.clone());
        }
        let video_id = track.video_id.as_deref()?;
        let song = known.get(video_id)?.as_ref()?;
        ytmusic::dedup::best_song_match(track, vec![song.clone()]).map(Some)
    }

    /// The song behind a video row the listener asked for. A video whose real runtime is the one
    /// the listing showed already plays the song's audio — YouTube's own listings put the song's
    /// runtime under the video — so it is left alone; asking a search and pointing it at another
    /// upload would only risk a different recording. A video that really runs longer carries
    /// something the song does not, an intro or a spoken passage, and plays the song instead.
    pub async fn for_playing(self: &Arc<Self>, track: Track) -> Track {
        if !track.is_video() || track.video_id.is_none() {
            return track;
        }
        if let Some(known) = self.cached(&track) {
            return match known {
                Some(song) => applied(track, song),
                None => track,
            };
        }
        let song = match self.ask(&track).await {
            Answer::Song(song) => *song,
            Answer::None => {
                self.record(&track, None);
                return track;
            }
            Answer::Failed => return track,
        };
        // Hold the song to the runtime the video really has. A video no longer than its song is
        // playing the song's audio already — a lyric video, a visualizer, an "official audio" —
        // and pointing it at another upload would only risk a different recording. One that runs
        // longer carries something the song has not, so what plays is the song.
        let longer = match (song.duration, self.video_duration(&track).await) {
            (Some(song), Some(video)) => video.saturating_sub(song) > TOLERANCE,
            _ => true,
        };
        match longer {
            true => {
                self.record(&track, Some(song.clone()));
                applied(track, song)
            }
            false => {
                log::debug!(
                    "youtube: {} runs no longer than its song, playing it as it is",
                    track.title
                );
                self.record(&track, None);
                track
            }
        }
    }

    /// The runtime YouTube serves a video at. Its own listings do not always tell it — an album
    /// shows the song's runtime under the video — so this asks, and answers `None` when it
    /// cannot be read.
    async fn video_duration(&self, track: &Track) -> Option<Duration> {
        let _permit = self.turn().await;
        self.api.track_duration(track.video_id.as_deref()?).await
    }

    /// The songs a listing holds as audio, read from the listing's own catalogue rows.
    ///
    /// A playlist or an album's backing playlist lists the catalogue songs, where a page that
    /// lists uploads lists their videos. Reading the catalogue listing is an answer rather than
    /// a guess, and it costs one request for the whole listing instead of a search per row.
    ///
    /// `None` when the listing cannot be read, or when it holds fewer rows than the page did
    /// and has no continuation — a partial listing is worse than none.
    pub async fn listing(&self, playlist_id: &str) -> Option<Vec<ytmusic::Track>> {
        let browse_id = format!("VL{}", playlist_id.trim_start_matches("VL"));
        let _permit = self.turn().await;
        let answer = self
            .api
            .execute(
                "browse",
                ytmusic::Client::Music,
                serde_json::json!({ "browseId": browse_id }),
            )
            .await
            .ok()?;
        let tracks: Vec<ytmusic::Track> =
            ytmusic::parse::find_renderers(&answer, "musicResponsiveListItemRenderer")
                .into_iter()
                .filter_map(ytmusic::parse::list_item_track)
                .collect();
        if tracks.is_empty() {
            return None;
        }
        Some(tracks)
    }

    /// The rows of a watch queue, which is how YouTube Music states a playlist's tracks: a row
    /// YouTube marks as audio is the song behind the video the playlist lists.
    ///
    /// The queue carries the type each row really is, so a queue of a playlist of official
    /// videos comes back with the songs already named — an answer rather than a search's guess.
    pub async fn queue(&self, playlist_id: &str) -> Option<Vec<ytmusic::Track>> {
        let _permit = self.turn().await;
        let answer = self
            .api
            .execute(
                "next",
                ytmusic::Client::Music,
                serde_json::json!({ "playlistId": playlist_id, "isAudioOnly": true }),
            )
            .await
            .ok()?;
        let tracks: Vec<ytmusic::Track> =
            ytmusic::parse::find_renderers(&answer, "playlistPanelVideoRenderer")
                .into_iter()
                .filter_map(ytmusic::parse::panel_track)
                .collect();
        if tracks.is_empty() {
            return None;
        }
        Some(tracks)
    }

    /// Waits for this request's turn: at most [`AT_ONCE`] in flight, and at least [`GAP`] apart.
    /// The permit is held for the length of the request it was taken for.
    async fn turn(&self) -> Option<tokio::sync::SemaphorePermit<'_>> {
        let permit = self.permits.acquire().await.ok()?;
        let mut start = self.start.lock().await;
        let wait = (*start + GAP).saturating_duration_since(Instant::now());
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        *start = Instant::now();
        Some(permit)
    }

    /// Searches for the song behind `track`, paced and with the guest credit stripped if the
    /// exact title finds nothing.
    async fn ask(&self, track: &Track) -> Answer {
        if self.quiet() {
            return Answer::Failed;
        }
        let Some(_permit) = self.turn().await else {
            return Answer::Failed;
        };
        self.ask_now(track).await
    }

    /// The search itself, for a caller that has already waited its turn.
    async fn ask_now(&self, track: &Track) -> Answer {
        match self.search_song(track).await {
            Ok(Searched::Song(song)) => {
                self.landed();
                return Answer::Song(song);
            }
            Ok(Searched::Nothing) => self.landed(),
            // A refusal is not a challenge, so it does not put the resolver to sleep — but it
            // says nothing about the song either, so nothing is remembered.
            Ok(Searched::Refused) => {
                log::warn!("youtube: {} could not be asked for", track.title);
                return Answer::Failed;
            }
            Err(error) => {
                self.failed(error);
                return Answer::Failed;
            }
        }
        // A video's title names its guests while the song's own title often leaves them out,
        // and the resolver compares titles exactly. Retry without them rather than play the
        // video behind the song.
        let guestless = without_guests(&track.title);
        if guestless.is_empty() || guestless == track.title {
            return Answer::None;
        }
        let mut retry = track.clone();
        retry.title = guestless;
        if self.quiet() {
            return Answer::Failed;
        }
        match self.search_song(&retry).await {
            Ok(Searched::Song(song)) => {
                self.landed();
                Answer::Song(song)
            }
            Ok(Searched::Nothing) => {
                self.landed();
                Answer::None
            }
            Ok(Searched::Refused) => Answer::Failed,
            Err(error) => {
                self.failed(error);
                Answer::Failed
            }
        }
    }

    /// Searches for the song behind `track`.
    ///
    /// The search goes out signed in: the guest search is the one Google answers with a
    /// challenge first, and a challenge is not an answer.
    ///
    /// Google refuses some searches outright — it answers `400 invalid argument` for the ones
    /// that read like a list of keywords — so the row is asked about in several ways and the
    /// first one Google answers is the one that counts. Only an answer to the exact question
    /// says there is no song: a simpler question answers about a different thing, so its
    /// silence is not a verdict.
    async fn search_song(&self, track: &Track) -> Result<Searched> {
        let mut refused = None;
        for (index, question) in questions(track).into_iter().enumerate() {
            match self.api.search_songs(&question).await {
                Ok(candidates) => {
                    if let Some(song) = ytmusic::dedup::best_song_match(track, candidates) {
                        return Ok(Searched::Song(Box::new(song)));
                    }
                    // The search answered and the song is not in it. Only the exact question can
                    // say that; a simpler one answers about a different thing.
                    return Ok(match index {
                        0 => Searched::Nothing,
                        _ => Searched::Refused,
                    });
                }
                Err(failure) => {
                    let refused_here = failure.to_string().contains("400 Bad Request");
                    log::debug!("youtube: the search for {question:?} was refused: {failure:#}");
                    if !refused_here {
                        return Err(failure);
                    }
                    refused = Some(failure);
                }
            }
        }
        match refused {
            Some(error) => Err(error),
            None => Ok(Searched::Nothing),
        }
    }

    /// Whether a run of failures has put the resolver to sleep. Lifts the sleep when it is over.
    fn quiet(&self) -> bool {
        let Ok(mut until) = self.quiet_until.lock() else {
            return false;
        };
        match *until {
            Some(at) if Instant::now() < at => true,
            Some(_) => {
                *until = None;
                false
            }
            None => false,
        }
    }

    fn landed(&self) {
        self.failures.store(0, Ordering::Relaxed);
    }

    fn failed(&self, error: anyhow::Error) {
        let count = self.failures.fetch_add(1, Ordering::Relaxed) + 1;
        log::warn!("youtube: cannot resolve a song: {error:#}");
        if count < PATIENCE {
            return;
        }
        if let Ok(mut until) = self.quiet_until.lock() {
            *until = Some(Instant::now() + QUIET);
        }
        self.failures.store(0, Ordering::Relaxed);
        log::warn!(
            "youtube: searches are being refused, leaving videos as they are for {} minutes",
            QUIET.as_secs() / 60
        );
    }

    /// Remembers what a completed search said, and rewrites the file at most every
    /// [`WRITE_EVERY`].
    fn record(&self, track: &Track, song: Option<Track>) {
        if let Ok(mut known) = self.known.lock() {
            known.insert(key_of(track), song.clone());
            // The bare video id serves a row that has no entry of its own yet, as long as the
            // song it holds is the song that row would have found.
            if let Some(song) = song
                && let Some(video_id) = track.video_id.as_deref()
                && !known.contains_key(video_id)
            {
                known.insert(video_id.to_owned(), Some(song));
            }
        }
        let Ok(mut written) = self.written.lock() else {
            return;
        };
        if written.elapsed() < WRITE_EVERY {
            return;
        }
        *written = Instant::now();
        drop(written);
        self.write();
    }

    /// Replaces the cache file in one step, so a crash cannot leave it half written.
    fn write(&self) {
        let Ok(known) = self.known.lock().map(|known| known.clone()) else {
            return;
        };
        let Ok(body) = serde_json::to_vec(&known) else {
            return;
        };
        let temporary = self.path.with_extension("json.tmp");
        if let Some(parent) = self.path.parent()
            && let Err(error) = fs::create_dir_all(parent)
        {
            log::warn!("youtube: cannot create the resolution cache folder: {error}");
            return;
        }
        if let Err(error) =
            fs::write(&temporary, body).and_then(|()| fs::rename(&temporary, &self.path))
        {
            log::warn!("youtube: cannot write the resolution cache: {error}");
        }
    }
}

/// The row's own key: the video it is, its title and its credits. Two rows can share a video
/// and still want different songs, and each keeps its own answer.
fn key_of(track: &Track) -> String {
    let mut artists: Vec<String> = track
        .artists
        .iter()
        .map(|artist| artist.name.to_lowercase())
        .collect();
    artists.sort();
    format!(
        "{}\u{1}{}\u{1}{}",
        track.video_id.as_deref().unwrap_or_default(),
        track.title.to_lowercase(),
        artists.join(", ")
    )
}

/// Takes the song's playable id and runtime onto the listing's row.
fn applied(mut track: Track, song: Track) -> Track {
    if song.video_id.is_some() {
        track.video_id = song.video_id;
    }
    if song.duration.is_some() {
        track.duration = song.duration;
    }
    track.kind = TrackKind::Song;
    if track.album.is_none() {
        track.album = song.album;
    }
    track
}

/// A guest credit as a title writes it, `(feat. …)`, `[ft. …]`, `(featuring …)`.
static GUEST_CREDIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*[\(\[](?:feat\.|feat\s|ft\.|ft\s|featuring\s)[^)\]]*[\)\]]").unwrap()
});

/// The title with its guest credits taken out, for matching against a song whose own title
/// leaves the guests out.
fn without_guests(title: &str) -> String {
    GUEST_CREDIT.replace_all(title, "").trim().to_owned()
}

/// A note in a title that only says the row is a video, as `(Official Music Video)` or
/// `(Lyric Video)` writes it.
static VIDEO_NOTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\s*[\(\[](?:[^)\]]*(?:official video|official music video|official audio|official lyric video|lyric video|lyrics video|music video|visualizer|audio|mv)[^)\]]*[\)\]])",
    )
    .unwrap()
});

/// The questions to ask for the song behind a row, from the most exact to the simplest.
///
/// Google refuses some searches outright — it answers `400 invalid argument` for the ones that
/// read like a list of keywords rather than a title — and a shorter question is often accepted
/// where the long one was not. So the row is asked about in several ways, and the first one
/// Google answers is the one that counts.
fn questions(track: &Track) -> Vec<String> {
    let title = VIDEO_NOTE.replace_all(&track.title, " ").into_owned();
    let artists: Vec<String> = track
        .artists
        .iter()
        .map(|artist| artist.name.clone())
        .filter(|name| !name.trim().is_empty())
        .collect();
    let mut out: Vec<String> = Vec::new();
    let mut add = |question: String| {
        let question = tidy(&question);
        if !question.is_empty() && !out.contains(&question) {
            out.push(question);
        }
    };
    add(match artists.is_empty() {
        true => title.clone(),
        false => format!("{title} {}", artists.join(" ")),
    });
    add(title.clone());
    // A title that reads like a list is the one Google refuses, so it is also asked about with
    // only its first few words.
    let words: Vec<&str> = title.split_whitespace().collect();
    for count in [4usize, 3, 2] {
        if words.len() <= count {
            break;
        }
        let short = words[..count].join(" ");
        add(match artists.first() {
            Some(artist) => format!("{short} {artist}"),
            None => short.clone(),
        });
        add(short);
    }
    out
}

/// A query as it goes out: no control characters, no runs of space.
fn tidy(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Where the resolver keeps what it has learned.
pub fn path() -> PathBuf {
    crate::credentials::root()
        .join("youtube")
        .join("resolved.json")
}
