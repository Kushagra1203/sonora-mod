//! Addons: community HTTP servers that find a track and hand back a playable URL, in the
//! protocol BitChord documents — `/manifest.json`, `/search?q=`, `/stream/{id}`.
//!
//! This provider is search and playback and nothing else: no library, no playlists, no
//! artist pages. It is the one way to reach a lossless stream without a sign-in, which is
//! exactly what the addon ecosystem exists for. Anything the protocol cannot answer bails
//! with a message rather than pretending.
//!
//! The addon list is a JSON file under the provider's own accounts folder, written through
//! `credentials::write`; a session needs no sign-in beyond that file existing. Playback is
//! progressive FLAC/MP3 straight off the URL an addon resolves; DASH streams (Tidal hi-res)
//! are refused with a message rather than played wrong, since the segmented path is a later
//! piece of work.

pub(crate) mod client;
mod playback;
mod store;
mod wire;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use futures::StreamExt as _;
use md5::{Digest as _, Md5};

use self::client::{AddonClient, Tier};
use self::wire::{AddonStream, AddonTrack};
use crate::{
    Capabilities, Catalog, Encoding, InputSource, MediaKind, MusicApi, MusicProvider, Offer,
    PlaybackFactory, PromptSink, ProviderSession, Quality, Shape, SignIn, Track, UserProfile,
    models::{Album, AlbumDetail, Artist, ArtistProfile, Playlist, PlaylistDetail, SavedArtist},
};

/// The levels an addon is asked for. Lossless is the point of the provider; the lower two
/// exist because the protocol defines them and an addon without lossless still works.
const OFFERS: &[Offer] = &[Offer {
    quality: Quality::Lossless,
    encoding: Encoding::Flac {
        depth: 16,
        rate: 44_100,
    },
    up_to: true,
}];

/// The marker file that says the listener chose this provider. The addon list alone is
/// configuration and does not mean the provider was selected; this does, so a restart after
/// signing out does not restore it.
const ACTIVE: &str = "active";

/// How long the upgrade search may take on one source before it is walked past. Something is
/// already playing, so a slow answer is worth less than a quick miss; the searches run at once
/// and the first usable one wins.
const UPGRADE_SEARCH: Duration = Duration::from_secs(8);
/// How long resolving one candidate stream may take.
const UPGRADE_STREAM: Duration = Duration::from_secs(12);

/// Everything one session shares: the addon list in priority order, the rows a search has
/// answered with (remembered only for the life of the process, so `track` and a row's own
/// `streamURL` need no second round trip), all of it reloadable so edits the settings screen
/// makes are picked up without rebuilding the session.
pub(crate) struct Shared {
    addons: std::sync::RwLock<Vec<Arc<Addon>>>,
    rows: Mutex<HashMap<String, AddonTrack>>,
    /// Streams an upgrade resolved and verified, waiting for the engine to open them. Taken
    /// rather than copied, since an addon URL may be single-use or short-lived.
    streams: Mutex<HashMap<String, AddonStream>>,
}

/// One configured addon with its live client.
pub(crate) struct Addon {
    pub entry: store::Entry,
    /// A short, stable name for this addon inside a track id. It comes from the address, so
    /// reordering the list never makes an old track id point at a different addon.
    pub key: String,
    pub client: AddonClient,
}

/// The stable key a track id carries for the addon at `url`.
fn key_of(url: &str) -> String {
    let digest = format!("{:x}", Md5::digest(url.as_bytes()));
    digest[..12].to_owned()
}

impl Shared {
    fn new(entries: Vec<store::Entry>) -> Self {
        Self {
            addons: std::sync::RwLock::new(build(entries)),
            rows: Mutex::new(HashMap::new()),
            streams: Mutex::new(HashMap::new()),
        }
    }

    /// Remembers a search row under its full track id, for `track` and for a row that
    /// carries its own `streamURL`.
    fn remember(&self, full_id: String, row: &AddonTrack) {
        self.rows
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(full_id, row.clone());
    }

    /// The row a full track id came from, while this session still knows it.
    fn recall(&self, full_id: &str) -> Option<AddonTrack> {
        self.rows
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(full_id)
            .cloned()
    }

    /// Keeps a stream an upgrade already resolved, so opening the track does not ask the addon
    /// again and risk a second, colder answer.
    fn remember_stream(&self, full_id: String, stream: AddonStream) {
        self.streams
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(full_id, stream);
    }

    /// The stream resolved for `full_id`, once. A later replay resolves it afresh.
    fn take_stream(&self, full_id: &str) -> Option<AddonStream> {
        self.streams
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(full_id)
    }

    /// Drops a resolved stream an upgrade no longer needs.
    fn forget_stream(&self, full_id: &str) {
        self.streams
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(full_id);
    }

    /// Whether any source is switched on, so an upgrade has somewhere to ask.
    fn ready(&self) -> bool {
        self.sync();
        self.snapshot().iter().any(|addon| addon.entry.enabled)
    }

    /// The addon a track id names. Ids written before keys were stable carried the list
    /// position instead, which is still read as a fallback so old links keep playing.
    pub(crate) fn addon(&self, key: &str) -> Option<Arc<Addon>> {
        let addons = self.snapshot();
        addons
            .iter()
            .find(|addon| addon.key == key)
            .cloned()
            .or_else(|| {
                let index = key.parse::<usize>().ok()?;
                addons.get(index).cloned()
            })
    }

    /// The live list. Cloned out so a caller can await per addon without holding the lock.
    pub(crate) fn snapshot(&self) -> Vec<Arc<Addon>> {
        self.addons
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Picks up edits the settings screen made: rebuilds the clients when the stored list is
    /// no longer the one held. A URL that no longer parses is skipped with a log line rather
    /// than failing the whole provider.
    pub(crate) fn sync(&self) {
        let entries = store::load();
        {
            let current = self.addons.read().unwrap_or_else(|p| p.into_inner());
            if current.len() == entries.len()
                && current
                    .iter()
                    .zip(&entries)
                    .all(|(addon, entry)| addon.entry == *entry)
            {
                return;
            }
        }
        let rebuilt = build(entries);
        *self.addons.write().unwrap_or_else(|p| p.into_inner()) = rebuilt;
    }
}

/// Builds the client list for a stored list, skipping any URL that will not parse.
fn build(entries: Vec<store::Entry>) -> Vec<Arc<Addon>> {
    let mut addons = Vec::with_capacity(entries.len());
    for entry in entries {
        match AddonClient::new(&entry.url) {
            Ok(client) => addons.push(Arc::new(Addon {
                key: key_of(&entry.url),
                entry,
                client,
            })),
            Err(error) => log::warn!("addons: skipping {}: {error:#}", client::redact(&entry.url)),
        }
    }
    addons
}

// ── What the settings screen reads and writes ─────────────────────────────────
// Re-exported from `music`'s root, since state and views reach a provider only through it.

/// One configured addon, as the settings screen sees it: the URL, what to call it, whether
/// it is switched on.
pub type AddonEntry = store::Entry;

/// The configured addons, in the order they are tried.
pub fn addon_list() -> Vec<AddonEntry> {
    store::load()
}

/// Stores the list. A live session picks the change up on its next search or load.
pub fn addon_save(entries: &[AddonEntry]) -> Result<()> {
    store::save(entries)
}

/// What asking a URL for its manifest learned worth showing.
pub struct AddonProbe {
    /// What the addon calls itself.
    pub name: String,
    pub version: String,
    /// Whether the addon refuses to serve while the output cannot carry lossless audio. On a
    /// desktop that is always false in practice, but the settings screen says so when not.
    pub lossless_gated: bool,
}

/// Asks a URL for its manifest, so the settings screen can say an address works before it is
/// stored. Network: callers run this on the io pool.
pub async fn addon_probe(url: &str) -> Result<AddonProbe> {
    let client = AddonClient::new(url)?;
    let manifest = client.manifest().await?;
    Ok(AddonProbe {
        name: manifest.display_name(),
        version: manifest.version.clone(),
        lossless_gated: manifest.requires_lossless_output(),
    })
}

/// Finds a lossless addon copy of a track playing through another provider, and builds the
/// engine that plays it. This is what lets a stream start on YouTube and become lossless a
/// moment later without the listener choosing a second source.
///
/// The handle owns only the configured sources; it needs no provider session, so it works
/// while another service is the one in use.
pub struct AddonUpgrade {
    shared: Arc<Shared>,
}

impl AddonUpgrade {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared::new(store::load())),
        }
    }

    /// A playback factory over the same addon list, for the engine that shadows the stream.
    pub fn factory(&self) -> Arc<dyn PlaybackFactory> {
        Arc::new(playback::Factory::new(self.shared.clone()))
    }

    /// Whether any source is switched on.
    pub fn available(&self) -> bool {
        self.shared.ready()
    }

    /// Drops a stream a swap resolved but will not use, so a later replay asks the addon again
    /// instead of opening a URL that may already have expired.
    pub fn forget_stream(&self, id: &str) {
        self.shared.forget_stream(id);
    }

    /// A lossless copy of `track` that an enabled addon holds and can stream progressively.
    /// Every source is asked at once and the first usable answer wins, so a dead or cold addon
    /// cannot hold a fast one back; among the rows of one addon a hi-res rendition is tried
    /// before a CD one. The stream is resolved and checked here so an addon that answers lossy,
    /// hands back a segmented manifest or is DRM-protected never causes a swap. Network:
    /// callers run this off the main thread.
    pub async fn find(&self, track: &Track) -> Option<Track> {
        let title = track.name.trim();
        if title.is_empty() {
            return None;
        }
        let query = match track.artists.trim() {
            "" => title.to_owned(),
            artists => format!("{title} {artists}"),
        };
        self.shared.sync();
        let addons: Vec<Arc<Addon>> = self
            .shared
            .snapshot()
            .into_iter()
            .filter(|addon| addon.entry.enabled)
            .collect();
        if addons.is_empty() {
            return None;
        }

        let mut searches =
            futures::stream::FuturesUnordered::from_iter(addons.into_iter().map(|addon| {
                let query = query.clone();
                ask(addon, query)
            }));

        while let Some((addon, rows)) = searches.next().await {
            let rows = match rows {
                Ok(rows) => rows,
                Err(error) => {
                    log::warn!(
                        "addons: upgrade search on {} failed: {error:#}",
                        client::redact(addon.client.base())
                    );
                    continue;
                }
            };
            let mut candidates: Vec<&AddonTrack> =
                rows.iter().filter(|row| matches(track, row)).collect();
            candidates.sort_by_key(|row| !row.is_hi_res());
            for row in candidates {
                match addon
                    .client
                    .stream_within(&row.id, Tier::HiRes, UPGRADE_STREAM)
                    .await
                {
                    Ok(Some(stream))
                        if stream.is_lossless()
                            && stream.transport().is_none()
                            && !stream.is_encrypted() =>
                    {
                        let found = track_of(&self.shared, &addon, &row);
                        if let Some(id) = found.id.clone() {
                            self.shared.remember_stream(id, stream);
                        }
                        log::info!(
                            "addons: upgrading {} to a lossless copy from {}",
                            track.name,
                            client::redact(addon.client.base())
                        );
                        return Some(found);
                    }
                    Ok(_) => {}
                    Err(error) => log::warn!(
                        "addons: cannot resolve the upgrade of {} on {}: {error:#}",
                        track.name,
                        client::redact(addon.client.base())
                    ),
                }
            }
        }
        None
    }
}

impl Default for AddonUpgrade {
    fn default() -> Self {
        Self::new()
    }
}

/// Addons as the app sees them before one is chosen: nothing to sign in with, a list to read.
pub struct AddonProvider;

impl AddonProvider {
    pub fn new() -> Self {
        Self
    }

    /// The marker path.
    fn active_path() -> std::path::PathBuf {
        crate::credentials::dir("addons").join(ACTIVE)
    }

    /// Whether the listener chose this provider before.
    fn active() -> bool {
        Self::active_path().exists()
    }

    /// Opens the stored state. `None` when the provider was never chosen, so the session
    /// walks on to the next provider exactly as it does for one with no account.
    async fn restore_stored() -> Result<Option<ProviderSession>> {
        if !Self::active() {
            return Ok(None);
        }
        Ok(Some(session()?))
    }
}

impl Default for AddonProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MusicProvider for AddonProvider {
    fn name(&self) -> &'static str {
        "Addons"
    }

    fn slug(&self) -> &'static str {
        "addons"
    }

    fn listening_to(&self) -> &'static str {
        "Addons"
    }

    fn sign_in_options(&self) -> Vec<SignIn> {
        vec![SignIn::Anonymous]
    }

    /// Addons are not an account service: there is nobody to sign in as, and the sources are
    /// managed on their own settings page rather than in the streaming-services list.
    fn account_service(&self) -> bool {
        false
    }

    fn stored(&self) -> bool {
        Self::active()
    }

    /// Whether what is stored is an anonymous session rather than an account. It always is:
    /// addons have no accounts at all.
    fn stored_guest(&self) -> bool {
        true
    }

    async fn restore(&self) -> Result<Option<ProviderSession>> {
        Self::restore_stored().await
    }

    async fn sign_in(
        &self,
        method: SignIn,
        _prompt: PromptSink,
        _input: InputSource,
    ) -> Result<ProviderSession> {
        match method {
            SignIn::Anonymous | SignIn::Default => {}
            _ => bail!("addons have no sign-in of their own"),
        }
        let session = session()?;
        let path = Self::active_path();
        crate::credentials::write(&path, b"true").context("cannot remember the addon choice")?;
        Ok(session)
    }

    fn sign_out(&self) {
        let path = Self::active_path();
        if let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("addons: cannot forget the choice: {error}");
        }
    }

    /// Reads one of this provider's own links back into the track it names. Only the addon
    /// scheme is our own; a link of any other provider's is left for them.
    fn link(&self, link: &str) -> Option<crate::link::Linked> {
        let rest = link.trim().strip_prefix(SCHEME)?;
        parse_id(rest)?;
        Some(crate::link::Linked::Track(rest.to_owned()))
    }
}

/// The scheme every addon link travels under. Not `sonora://`, which is Sonora's own for
/// themes, so a pasted addon link falls through to this provider rather than the theme
/// reader.
const SCHEME: &str = "addon://";

/// Builds the session every open of this provider produces, from the stored list.
fn session() -> Result<ProviderSession> {
    let shared = Arc::new(Shared::new(store::load()));
    Ok(ProviderSession {
        profile: UserProfile {
            id: "addons".into(),
            display_name: "Addons".into(),
            avatar: None,
        },
        api: Arc::new(AddonApi {
            shared: shared.clone(),
        }),
        playback: Arc::new(playback::Factory::new(shared)),
        shape: Shape::Catalog(Catalog::Fixed),
        authenticated: false,
        capabilities: Capabilities {
            follow_artists: false,
            radio: false,
            playcounts: false,
            pins: false,
            transcode: false,
            playlist_covers: false,
            keeps: false,
            downloads: false,
            qualities: OFFERS,
            download_gap: Duration::ZERO,
        },
    })
}

/// Search and track lookup over every configured addon, in priority order.
struct AddonApi {
    shared: Arc<Shared>,
}

impl AddonApi {
    /// The full track id this provider hands out, which playback parses back. The key names
    /// the addon by its address, so reordering the list leaves every old id pointing at the
    /// addon it came from.
    fn full_id(key: &str, track_id: &str) -> String {
        format!("addon:{key}:{track_id}")
    }

    /// Maps one row of one addon into a track, remembering the row so `track` can answer for
    /// it again without a second search.
    fn to_track(&self, addon: &Addon, row: &AddonTrack) -> Track {
        track_of(&self.shared, addon, row)
    }

    /// The track a search row became. Addons answer no by-id lookup of their own, so a track
    /// outside this session's memory is one the listener has to search for again.
    async fn track(&self, track_id: &str) -> Result<Track> {
        let (key, _) = parse_id(track_id).context("that is not an addon track id")?;
        self.shared.sync();
        let Some(addon) = self.shared.addon(key) else {
            bail!("the addon that held this track is no longer configured");
        };
        let Some(row) = self.shared.recall(track_id) else {
            bail!("search for this track again; addons cannot look a track up by id");
        };
        Ok(self.to_track(&addon, &row))
    }
}

#[async_trait]
impl MusicApi for AddonApi {
    /// A track's own link, in the addon scheme: the track id under `addon://`. Nothing but a
    /// track has one, since addon rows are tracks; the link exists so a hit can be handed to
    /// `play`, `add` and the clipboard, which every one of them reads back through
    /// [`AddonProvider::link`].
    fn share_url(&self, kind: MediaKind, id: &str) -> Option<String> {
        match kind {
            MediaKind::Track => Some(format!("addon://{id}")),
            MediaKind::Album | MediaKind::Artist | MediaKind::Playlist => None,
        }
    }

    async fn profile(&self) -> Result<UserProfile> {
        Ok(UserProfile {
            id: "addons".into(),
            display_name: "Addons".into(),
            avatar: None,
        })
    }

    async fn artist(&self, _artist_id: &str) -> Result<Artist> {
        bail!("addons have no artist pages")
    }

    async fn artist_profile(&self, _artist_id: &str) -> Result<ArtistProfile> {
        bail!("addons have no artist pages")
    }

    async fn artist_images(&self, _ids: Vec<String>) -> Result<HashMap<String, String>> {
        Ok(HashMap::new())
    }

    async fn saved_tracks(&self) -> Result<Vec<Track>> {
        Ok(Vec::new())
    }

    async fn set_track_saved(&self, _track_id: &str, _saved: bool) -> Result<()> {
        Ok(())
    }

    /// The track a search row became, read back out of this session's memory of it.
    async fn track(&self, track_id: &str) -> Result<Track> {
        AddonApi::track(self, track_id).await
    }

    async fn track_playcount(&self, _track_id: &str) -> Result<Option<u64>> {
        Ok(None)
    }

    async fn playlists(&self) -> Result<Vec<Playlist>> {
        Ok(Vec::new())
    }

    async fn create_playlist(&self, _name: &str) -> Result<String> {
        bail!("addons cannot keep playlists")
    }

    async fn rename_playlist(&self, _playlist_id: &str, _name: &str) -> Result<()> {
        bail!("addons cannot keep playlists")
    }

    async fn delete_playlist(&self, _playlist_id: &str) -> Result<()> {
        bail!("addons cannot keep playlists")
    }

    async fn remove_playlist_from_library(&self, _playlist_id: &str) -> Result<()> {
        bail!("addons cannot keep playlists")
    }

    async fn add_playlist_to_library(&self, _playlist_id: &str) -> Result<()> {
        bail!("addons cannot keep playlists")
    }

    async fn set_playlist_public(&self, _playlist_id: &str, _public: bool) -> Result<()> {
        bail!("addons cannot keep playlists")
    }

    async fn add_track_to_playlist(&self, _playlist_id: &str, _track_id: &str) -> Result<()> {
        bail!("addons cannot keep playlists")
    }

    async fn remove_track_from_playlist(&self, _playlist_id: &str, _track_id: &str) -> Result<()> {
        bail!("addons cannot keep playlists")
    }

    async fn saved_albums(&self) -> Result<Vec<Album>> {
        Ok(Vec::new())
    }

    async fn set_album_saved(&self, _album_id: &str, _saved: bool) -> Result<()> {
        bail!("addons have no albums to keep")
    }

    async fn saved_artists(&self) -> Result<Vec<SavedArtist>> {
        Ok(Vec::new())
    }

    async fn set_artist_saved(&self, _artist_id: &str, _saved: bool) -> Result<()> {
        bail!("addons have no artist pages")
    }

    async fn album(&self, _album_id: &str) -> Result<AlbumDetail> {
        bail!("addons have no albums")
    }

    async fn album_tracks(&self, _album_id: &str) -> Result<Vec<Track>> {
        bail!("addons have no albums")
    }

    async fn playlist(&self, _playlist_id: &str) -> Result<PlaylistDetail> {
        bail!("addons have no playlists")
    }

    async fn playlist_tracks(&self, _playlist_id: &str) -> Result<Vec<Track>> {
        bail!("addons have no playlists")
    }

    async fn playlist_covers(&self, _playlist_id: &str, _wanted: usize) -> Result<Vec<String>> {
        bail!("addons have no playlists")
    }

    async fn track_radio(
        &self,
        _track_id: &str,
        _from: Option<&str>,
    ) -> Result<(Vec<Track>, Option<String>)> {
        bail!("addons cannot seed a station")
    }

    /// Every enabled addon is asked in priority order and the answers are merged, first
    /// addon winning a duplicate. A failure from one addon is logged and walked past: the
    /// listener searched, and one dead server should not swallow the other five.
    async fn search(&self, query: &str) -> Result<Vec<Track>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        // Edits the settings screen made take effect from this search on.
        self.shared.sync();
        let addons = self.shared.snapshot();
        let mut tracks: Vec<Track> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for addon in &addons {
            if !addon.entry.enabled {
                continue;
            }
            match addon.client.search(query, Tier::Lossless).await {
                Ok(rows) => {
                    for row in rows {
                        if row.id.trim().is_empty() || row.title.trim().is_empty() {
                            continue;
                        }
                        let key = format!(
                            "{}\u{1}{}",
                            row.title.trim().to_lowercase(),
                            row.artist.trim().to_lowercase()
                        );
                        if !seen.insert(key) {
                            continue;
                        }
                        tracks.push(self.to_track(addon, &row));
                    }
                }
                Err(error) => {
                    log::warn!(
                        "addons: search on {} failed: {error:#}",
                        client::redact(addon.client.base())
                    );
                }
            }
        }
        Ok(tracks)
    }
}

/// Splits a full track id back into the addon key and the addon's own id. A numeric key is
/// an id from before keys were stable, and the addon lookup reads it as a list position.
fn parse_id(track_id: &str) -> Option<(&str, &str)> {
    let rest = track_id.strip_prefix("addon:")?;
    let (key, id) = rest.split_once(':')?;
    Some((key, id))
}

/// Maps one search row to the track this provider hands out, remembering the row so `track`
/// can answer for it again without a second search.
fn track_of(shared: &Shared, addon: &Addon, row: &AddonTrack) -> Track {
    let full_id = AddonApi::full_id(&addon.key, &row.id);
    shared.remember(full_id.clone(), row);
    Track {
        id: Some(full_id),
        name: row.title.clone(),
        playable: true,
        artists: row.artist.clone(),
        artist_refs: vec![crate::models::ArtistRef {
            name: row.artist.clone(),
            id: None,
        }],
        album: row.album.clone(),
        album_id: None,
        cover: row.artwork().map(str::to_owned),
        duration: Duration::from_secs(row.duration_secs().unwrap_or(0)),
        added_at: None,
        added_by: None,
        playcount: None,
        popularity: 0,
        explicit: false,
        track_number: 0,
        disc_number: 0,
        tags: Vec::new(),
        languages: Vec::new(),
        credits: Vec::new(),
    }
}

/// Whether an addon row is the same recording as `track`, as far as a title, artist and a
/// roughly equal length can say. The comparison ignores case and punctuation, and an artist
/// field listing several names matches when one of them does.
fn matches(track: &Track, row: &AddonTrack) -> bool {
    if title_key(&track.name) != title_key(&row.title) {
        return false;
    }
    let row_artist = word_key(&row.artist);
    let artists = word_key(&track.artists);
    if !row_artist.is_empty()
        && !artists.is_empty()
        && !(row_artist == artists
            || row_artist.contains(&artists)
            || artists.contains(&row_artist))
    {
        return false;
    }
    match (track.duration.as_secs(), row.duration_secs()) {
        (0, _) | (_, None) => true,
        (wanted, Some(found)) => wanted.abs_diff(found) <= 15,
    }
}

/// Asks one addon for rows that may be `query`, carrying the addon along with its answer so
/// the caller knows which source answered without keeping the list around.
async fn ask(addon: Arc<Addon>, query: String) -> (Arc<Addon>, Result<Vec<AddonTrack>>) {
    let rows = addon
        .client
        .search_within(&query, Tier::HiRes, UPGRADE_SEARCH)
        .await;
    (addon, rows)
}

/// A title as the words that decide which recording it is. A bracketed qualifier is a
/// presentation note (a featuring credit, a soundtrack, a video tag) and falls away, unless it
/// names a version — a remix, an edit, an instrumental — which is a different recording and
/// must match only itself. Unbracketed featuring credits fall away the same way. So
/// `Dracula (feat. JENNIE)` matches `Dracula`, while `like JENNIE (Peggy Gou Remix)` does not
/// match `like JENNIE`.
fn title_key(text: &str) -> String {
    let lower = text.to_lowercase();
    let mut outside = String::with_capacity(lower.len());
    let mut kept = String::new();
    let mut inside = String::new();
    let mut depth = 0usize;
    for character in lower.chars() {
        match character {
            '(' | '[' | '{' => {
                if depth == 0 {
                    inside.clear();
                }
                depth += 1;
                if depth == 1 {
                    continue;
                }
            }
            ')' | ']' | '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if is_version_qualifier(&inside) {
                        kept.push(' ');
                        kept.push_str(&inside);
                    }
                    inside.clear();
                    continue;
                }
            }
            _ => {}
        }
        match depth {
            0 => outside.push(character),
            _ => inside.push(character),
        }
    }
    let base = cut_features(&outside);
    let key = word_key(&format!("{base} {kept}"));
    // `Seven - Explicit Ver.` is the same recording as `Seven`; the edit markers every service
    // spells its own way carry no meaning once the words are compared. A title made only of
    // one of them keeps it, so a song actually called `Clean` is not left with no name.
    let trimmed = key
        .split(' ')
        .filter(|word| !matches!(*word, "explicit" | "clean" | "ver"))
        .collect::<Vec<_>>()
        .join(" ");
    match trimmed.is_empty() {
        true => key,
        false => trimmed,
    }
}

/// Whether a bracketed qualifier names a different recording. A qualifier with none of these
/// words is a credit or a note and does not distinguish one rendition from another.
const VERSION_WORDS: &[&str] = &[
    "remix",
    "rmx",
    "mix",
    "bootleg",
    "mashup",
    "edit",
    "extended",
    "slowed",
    "sped",
    "nightcore",
    "reverb",
    "instrumental",
    "karaoke",
    "cover",
    "live",
    "acoustic",
    "unplugged",
    "demo",
    "radio",
    "club",
];

/// Whether any word of `text` is one of [`VERSION_WORDS`].
fn is_version_qualifier(text: &str) -> bool {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .any(|word| VERSION_WORDS.contains(&word))
}

/// A title cut at the first featuring or production credit, which is how services credit the
/// same recording rather than a different one.
fn cut_features(text: &str) -> &str {
    let mut cut = text.len();
    for marker in [
        " feat",
        " ft.",
        " ft ",
        " featuring",
        " with ",
        " prod.",
        " produced by",
    ] {
        if let Some(index) = text.find(marker) {
            cut = cut.min(index);
        }
    }
    &text[..cut]
}

/// A text as lowercase words with the punctuation dropped, so `Yo-Sea` and `yo sea` compare
/// equal and a stray hyphen does not hide a match.
fn word_key(text: &str) -> String {
    let mut key = String::with_capacity(text.len());
    for character in text.chars().flat_map(char::to_lowercase) {
        match character.is_alphanumeric() {
            true => key.push(character),
            false if !key.ends_with(' ') && !key.is_empty() => key.push(' '),
            false => {}
        }
    }
    key.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_ids_round_trip() {
        let full = AddonApi::full_id("a1b2c3d4e5f6", "tidal:12345");
        assert_eq!(parse_id(&full), Some(("a1b2c3d4e5f6", "tidal:12345")));
        assert_eq!(parse_id("addon:0:x"), Some(("0", "x")));
        assert_eq!(parse_id("nope"), None);
    }

    #[test]
    fn keys_follow_the_address_not_the_order() {
        let first = key_of("https://one.example");
        let second = key_of("https://two.example");
        assert_eq!(first, key_of("https://one.example"));
        assert_ne!(first, second);
        assert_eq!(first.len(), 12);
    }
}
