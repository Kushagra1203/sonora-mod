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

use regex::Regex;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use tokio::task::JoinSet;
use ytmusic::{Track, TrackKind, YtMusic};

/// How many searches may be in flight together. A burst of six is what Google challenges.
const AT_ONCE: usize = 2;
/// The least time between two search requests, so even the two in flight stay a few a second.
const GAP: Duration = Duration::from_millis(250);
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

    /// Searches for the song behind `track`, paced and with the guest credit stripped if the
    /// exact title finds nothing.
    async fn ask(&self, track: &Track) -> Answer {
        if self.quiet() {
            return Answer::Failed;
        }
        let Ok(_permit) = self.permits.acquire().await else {
            return Answer::Failed;
        };
        {
            let mut start = self.start.lock().await;
            let wait = (*start + GAP).saturating_duration_since(Instant::now());
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
            *start = Instant::now();
        }
        match self.api.resolve_song(track).await {
            Ok(Some(song)) => {
                self.landed();
                return Answer::Song(Box::new(song));
            }
            Ok(None) => self.landed(),
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
        match self.api.resolve_song(&retry).await {
            Ok(Some(song)) => {
                self.landed();
                Answer::Song(Box::new(song))
            }
            Ok(None) => {
                self.landed();
                Answer::None
            }
            Err(error) => {
                self.failed(error);
                Answer::Failed
            }
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

/// Where the resolver keeps what it has learned.
pub fn path() -> PathBuf {
    crate::credentials::root()
        .join("youtube")
        .join("resolved.json")
}
