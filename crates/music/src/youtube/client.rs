use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use tokio::task::JoinSet;
use ytmusic::YtMusic;

use crate::cover::{self, CoverLimits};
use crate::youtube::resolve::{self, Resolver};
use crate::youtube::{genres, radio, subscriptions, wire};
use crate::{
    Album, AlbumCatalogue, AlbumDetail, Artist, ArtistCatalogue, ArtistProfile, CoverSize, Feed,
    Genre, GenreDetail, HomeFeed, MediaKind, MusicApi, Playlist, PlaylistDetail, SUGGESTIONS,
    SavedArtist, Track, UserProfile, escape,
};

const PORTRAIT_LIMIT: usize = 24;
/// YouTube documents no limit for a playlist thumbnail, so this stays near what its own upload
/// sends.
const COVER_LIMITS: CoverLimits = CoverLimits {
    side: 1200,
    bytes: 1024 * 1024,
};
const EPISODES: &str = "SE";
/// How many popular songs an artist page lists, even though its top songs page carries more.
const POPULAR: usize = 20;

pub struct YouTubeClient {
    api: Arc<YtMusic>,
    resolver: Arc<Resolver>,
    account: String,
}

impl YouTubeClient {
    pub fn new(api: Arc<YtMusic>) -> Self {
        let resolver = Arc::new(Resolver::new(api.clone(), resolve::path()));
        Self {
            api,
            resolver,
            account: String::new(),
        }
    }

    pub fn owned_by(mut self, account: impl Into<String>) -> Self {
        self.account = account.into();
        self
    }

    async fn hydrate_durations(&self, tracks: &mut [ytmusic::Track]) {
        let mut tasks = JoinSet::new();
        for (index, track) in tracks.iter().enumerate() {
            if track.duration.is_some() {
                continue;
            }
            let Some(video_id) = track.video_id.clone() else {
                continue;
            };
            let api = self.api.clone();
            tasks.spawn(async move { (index, api.track_duration(&video_id).await) });
        }
        while let Some(result) = tasks.join_next().await {
            if let Ok((index, Some(duration))) = result {
                tracks[index].duration = Some(duration);
            }
        }
    }

    /// The songs on the artist's "Top songs" page, which the artist page links to under its own
    /// five-song shelf.
    async fn top_songs(
        &self,
        browse_id: &str,
        params: Option<&str>,
    ) -> Result<Vec<ytmusic::Track>> {
        let mut payload = serde_json::json!({ "browseId": browse_id });
        if let Some(params) = params {
            payload["params"] = serde_json::Value::String(params.to_owned());
        }
        let page = wire::patience(|| {
            self.api
                .execute("browse", ytmusic::Client::Music, payload.clone())
        })
        .await?;
        Ok(wire::top_songs(&page))
    }

    /// The playlist's rows as YouTube's own watch queue states them, which is where it names the
    /// audio version of each row. A playlist of official videos comes back as the songs behind
    /// them, so one request replaces a search per row.
    ///
    /// `None` when the queue cannot be read or holds fewer rows than the playlist did, so a
    /// partial queue never replaces a whole listing.
    async fn playlist_audio(&self, playlist_id: &str, rows: usize) -> Option<Vec<ytmusic::Track>> {
        let tracks = self.resolver.queue(playlist_id).await?;
        if tracks.len() < rows {
            log::debug!(
                "youtube: the watch queue of {playlist_id} has {} rows against the playlist's {rows}",
                tracks.len()
            );
            return None;
        }
        Some(tracks)
    }

    /// The album's authoritative track listing: the backing playlist the release's own play
    /// action names, browsed directly, with the `VL` prefix a playlist browse needs. That
    /// playlist lists the catalogue songs where the album page shows the videos it has for them,
    /// so one request replaces a search per row.
    ///
    /// `None` when the album names no playlist, when the playlist cannot be read, or when it
    /// holds a poorer listing than the album's own rows — an album that stops at two hundred rows
    /// has no continuation, while its playlist pages a hundred at a time, so the playlist's rows
    /// only lose when they are both fewer and complete.
    async fn album_listing(&self, detail: &ytmusic::AlbumDetail) -> Option<Vec<ytmusic::Track>> {
        let playlist_id = detail.album.playlist_id.as_deref()?;
        let tracks = self.resolver.listing(playlist_id).await?;
        if tracks.len() < detail.tracks.len() {
            log::debug!(
                "youtube: album {playlist_id} has {} rows against the album's {}; keeping the album's own",
                tracks.len(),
                detail.tracks.len()
            );
            return None;
        }
        Some(tracks)
    }
}

#[async_trait]
impl MusicApi for YouTubeClient {
    fn cover_at(&self, cover: &str, size: CoverSize) -> Option<String> {
        wire::cover_at(cover, size)
    }

    fn share_url(&self, kind: MediaKind, id: &str) -> Option<String> {
        let id = escape::component(id);
        let url = match kind {
            MediaKind::Track => format!("https://music.youtube.com/watch?v={id}"),
            MediaKind::Album => format!("https://music.youtube.com/browse/{id}"),
            MediaKind::Artist => format!("https://music.youtube.com/channel/{id}"),
            MediaKind::Playlist => format!("https://music.youtube.com/playlist?list={id}"),
        };
        Some(url)
    }

    async fn profile(&self) -> Result<UserProfile> {
        Ok(wire::profile(self.api.profile().await?))
    }

    async fn song_behind(&self, track: Track) -> Result<Track> {
        Ok(wire::audio_only_playing(&self.resolver, track).await)
    }

    async fn artist(&self, artist_id: &str) -> Result<Artist> {
        // The crate reads the releases, the top songs and the header's texts. The page's own
        // response carries the counts and the shelves beside them, so it is read here too, side
        // by side with the crate's reading of the same page.
        let (base, page) = tokio::join!(
            wire::patience(|| self.api.artist(artist_id)),
            wire::patience(|| self.api.execute(
                "browse",
                ytmusic::Client::Music,
                serde_json::json!({ "browseId": artist_id }),
            )),
        );
        let mut base = base?;
        // The shelf's "Show all" leads to the artist's full top-songs list. The page itself only
        // carries five songs without lengths, so the full list is preferred and the crate's
        // per-song duration requests run only when it cannot be read.
        let mut full = None;
        if let Ok(page) = &page {
            for (browse_id, params) in wire::top_songs_endpoints(page) {
                match self.top_songs(&browse_id, params.as_deref()).await {
                    Ok(mut tracks) if !tracks.is_empty() => {
                        tracks.truncate(POPULAR);
                        full = Some(self.resolver.swap(tracks).await);
                        break;
                    }
                    Ok(_) => {}
                    Err(error) => log::debug!(
                        "artist: cannot read the top songs at {browse_id} of {artist_id}: {error:#}"
                    ),
                }
            }
            // A page whose full list cannot be read still carries songs of its own, and those
            // are better than a page that shows none.
            if full.is_none() {
                let inline = wire::top_songs_inline(page);
                if !inline.is_empty() {
                    let mut inline = inline;
                    inline.truncate(POPULAR);
                    full = Some(self.resolver.swap(inline).await);
                }
            }
        }
        if full.is_none() {
            self.hydrate_durations(&mut base.top_tracks).await;
            base.top_tracks = self.resolver.swap(base.top_tracks).await;
        }
        let mut mapped = wire::artist(base);
        if let Some(tracks) = full {
            mapped.top_tracks = tracks
                .into_iter()
                .enumerate()
                .map(|(index, track)| wire::track(track, index as u32))
                .collect();
        }
        match page {
            Ok(page) => wire::artist_page(&mut mapped, &page),
            Err(error) => log::debug!("artist: cannot read the shelves of {artist_id}: {error:#}"),
        }
        Ok(mapped)
    }

    /// The whole discography, since the artist page `artist` reads holds only ten albums and
    /// ten singles.
    async fn artist_catalogue(&self, artist_id: &str, _known: &[Track]) -> Result<ArtistCatalogue> {
        let albums = self
            .api
            .discography(artist_id)
            .await
            .with_context(|| format!("cannot load the discography of artist {artist_id}"))?;
        Ok(ArtistCatalogue {
            albums: albums.into_iter().map(wire::album).collect(),
            ..ArtistCatalogue::default()
        })
    }

    async fn artist_profile(&self, artist_id: &str) -> Result<ArtistProfile> {
        Ok(wire::artist_profile(&self.api.artist(artist_id).await?))
    }

    async fn artist_images(&self, ids: Vec<String>) -> Result<HashMap<String, String>> {
        let mut tasks = JoinSet::new();
        for id in ids.into_iter().take(PORTRAIT_LIMIT) {
            let api = self.api.clone();
            tasks.spawn(async move {
                let artist = api.artist(&id).await.ok()?;
                let image = wire::cover_large(&artist.thumbnails)?;
                Some((id, image))
            });
        }
        let mut images = HashMap::new();
        while let Some(result) = tasks.join_next().await {
            if let Ok(Some((id, image))) = result {
                images.insert(id, image);
            }
        }
        Ok(images)
    }

    async fn saved_tracks(&self) -> Result<Vec<Track>> {
        let liked = self.api.liked_songs().await?;
        let resolved = self.resolver.swap(liked).await;
        Ok(ytmusic::dedup::collapse(resolved)
            .into_iter()
            .enumerate()
            .map(|(index, track)| wire::track(track, index as u32))
            .collect())
    }

    async fn set_track_saved(&self, track_id: &str, saved: bool) -> Result<()> {
        self.api.rate_track(track_id, saved).await
    }

    async fn track(&self, track_id: &str) -> Result<Track> {
        let response = self
            .api
            .player_response(track_id, ytmusic::Client::Music)
            .await?;
        let details = response
            .get("videoDetails")
            .context("player response has no video details")?;
        let title = details
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let author = details
            .get("author")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let seconds: u64 = details
            .get("lengthSeconds")
            .and_then(serde_json::Value::as_str)
            .and_then(|length| length.parse().ok())
            .unwrap_or(0);
        let channel = details
            .get("channelId")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        // The player response says whether this id is the song or its video, so a video can be
        // swapped for its audio-only song before it ever reaches the engine.
        let kind = match details
            .get("musicVideoType")
            .and_then(serde_json::Value::as_str)
        {
            Some("MUSIC_VIDEO_TYPE_ATV") | None => ytmusic::TrackKind::Song,
            Some(_) => ytmusic::TrackKind::Video,
        };
        let source = ytmusic::Track {
            video_id: Some(track_id.to_string()),
            title,
            artists: vec![ytmusic::ArtistRef {
                name: author,
                id: channel,
            }],
            album: None,
            duration: Some(std::time::Duration::from_secs(seconds)),
            thumbnails: Vec::new(),
            explicit: false,
            available: true,
            kind,
            set_video_id: None,
            liked: None,
            views: None,
        };
        let mut track = wire::track(source, 0);
        track.cover = details
            .get("thumbnail")
            .map(|thumbnail| {
                let thumbs = collect_thumbnails(thumbnail);
                wire::cover(&thumbs)
            })
            .unwrap_or_default();
        // The same swap the listings get, so a video asked for on its own also plays its song.
        let mut tracks = wire::audio_only(&self.resolver, vec![track]).await;
        let Some(track) = tracks.pop() else {
            anyhow::bail!("track {track_id} could not be read");
        };
        Ok(track)
    }

    async fn track_playcount(&self, _track_id: &str) -> Result<Option<u64>> {
        Ok(None)
    }

    async fn playlists(&self) -> Result<Vec<Playlist>> {
        Ok(self
            .api
            .library_playlists()
            .await?
            .into_iter()
            .filter_map(|playlist| library_playlist(playlist, &self.account))
            .collect())
    }

    async fn create_playlist(&self, name: &str) -> Result<String> {
        self.api.create_playlist(name).await
    }

    async fn rename_playlist(&self, playlist_id: &str, name: &str) -> Result<()> {
        self.api.rename_playlist(playlist_id, name).await
    }

    async fn delete_playlist(&self, playlist_id: &str) -> Result<()> {
        self.api.delete_playlist(playlist_id).await
    }

    async fn remove_playlist_from_library(&self, playlist_id: &str) -> Result<()> {
        self.api.rate_playlist(playlist_id, false).await
    }

    async fn add_playlist_to_library(&self, playlist_id: &str) -> Result<()> {
        self.api.rate_playlist(playlist_id, true).await
    }

    async fn set_playlist_public(&self, playlist_id: &str, public: bool) -> Result<()> {
        self.api.set_playlist_privacy(playlist_id, public).await
    }

    async fn set_playlist_cover(&self, playlist_id: &str, image: Option<&[u8]>) -> Result<()> {
        match image {
            Some(image) => {
                let jpeg = cover::square_jpeg(image, COVER_LIMITS)?;
                self.api.set_playlist_thumbnail(playlist_id, jpeg).await
            }
            None => self.api.remove_playlist_thumbnail(playlist_id).await,
        }
    }

    async fn add_track_to_playlist(&self, playlist_id: &str, track_id: &str) -> Result<()> {
        self.api.add_playlist_track(playlist_id, track_id).await
    }

    async fn remove_track_from_playlist(&self, playlist_id: &str, track_id: &str) -> Result<()> {
        let tracks = self.api.playlist(playlist_id).await?.tracks;
        let seat = |list: &[ytmusic::Track]| {
            list.iter()
                .position(|track| track.video_id.as_deref() == Some(track_id))
        };
        let index = match seat(&tracks) {
            Some(index) => index,
            None => seat(&self.resolver.swap(tracks.clone()).await)
                .context("track is not in the playlist")?,
        };
        let track = tracks.get(index).context("track is not in the playlist")?;
        let video_id = track.video_id.as_deref().context("track has no video id")?;
        let set_video_id = track
            .set_video_id
            .as_deref()
            .context("track cannot be removed from this playlist")?;
        self.api
            .remove_playlist_track(playlist_id, video_id, set_video_id)
            .await
    }

    async fn saved_albums(&self) -> Result<Vec<Album>> {
        Ok(self
            .api
            .library_albums()
            .await?
            .into_iter()
            .map(wire::album)
            .collect())
    }

    async fn set_album_saved(&self, album_id: &str, saved: bool) -> Result<()> {
        let detail = self.api.album(album_id).await?;
        let playlist_id = detail
            .album
            .playlist_id
            .context("album has no audio playlist")?;
        self.api
            .rate_playlist(&playlist_id, saved)
            .await
            .with_context(|| format!("cannot rate the album {album_id} as {playlist_id}"))
    }

    async fn saved_artists(&self) -> Result<Vec<SavedArtist>> {
        subscriptions::saved(&self.api).await
    }

    async fn set_artist_saved(&self, artist_id: &str, saved: bool) -> Result<()> {
        subscriptions::set_saved(&self.api, artist_id, saved).await
    }

    async fn album(&self, album_id: &str) -> Result<AlbumDetail> {
        let mut detail = wire::patience(|| self.api.album(album_id)).await?;
        match self.album_listing(&detail).await {
            Some(tracks) => detail.tracks = tracks,
            None => detail.tracks = self.resolver.swap(detail.tracks).await,
        }
        Ok(wire::album_detail(detail))
    }

    async fn album_tracks(&self, album_id: &str) -> Result<Vec<Track>> {
        Ok(self.album(album_id).await?.tracks)
    }

    /// The artist's own releases without the album the page is already showing. Similar
    /// artists stay out: the client library reads no similarity off artist pages.
    async fn album_catalogue(
        &self,
        album_id: &str,
        artist_id: Option<&str>,
    ) -> Result<AlbumCatalogue> {
        let Some(artist_id) = artist_id else {
            return Ok(AlbumCatalogue::default());
        };
        let artist = wire::patience(|| self.api.artist(artist_id))
            .await
            .with_context(|| format!("cannot load more from artist {artist_id}"))?;
        let also_like = artist
            .albums
            .into_iter()
            .chain(artist.singles)
            .map(wire::album)
            .filter(|album| album.id != album_id)
            .take(SUGGESTIONS)
            .collect();
        Ok(AlbumCatalogue {
            also_like,
            similar: Vec::new(),
        })
    }

    async fn playlist(&self, playlist_id: &str) -> Result<PlaylistDetail> {
        let mut detail = wire::patience(|| self.api.playlist_page(playlist_id)).await?;
        match self.playlist_audio(playlist_id, detail.tracks.len()).await {
            Some(tracks) => detail.tracks = tracks,
            None => {
                // The listing's rows are videos — a mix or a radio — and the song behind each
                // one is a search away. Asking for every row before the page goes up holds it
                // back for a minute, so the rows go up as the listing gives them and the search
                // follows behind them.
                detail.tracks = self.resolver.known(std::mem::take(&mut detail.tracks));
                self.resolver.resolve_later(detail.tracks.clone());
            }
        }
        Ok(wire::playlist_detail(detail))
    }

    async fn playlist_continuation(
        &self,
        continuation: &str,
    ) -> Result<(Vec<Track>, Option<String>)> {
        let mut page = wire::patience(|| self.api.playlist_continuation(continuation)).await?;
        page.tracks = self.resolver.known(std::mem::take(&mut page.tracks));
        self.resolver.resolve_later(page.tracks.clone());
        Ok(wire::playlist_page(page))
    }

    async fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<Track>> {
        let mut detail = wire::patience(|| self.api.playlist(playlist_id)).await?;
        detail.tracks = self.resolver.known(std::mem::take(&mut detail.tracks));
        self.resolver.resolve_later(detail.tracks.clone());
        Ok(wire::playlist_detail(detail).tracks)
    }

    async fn playlist_covers(&self, playlist_id: &str, wanted: usize) -> Result<Vec<String>> {
        // The swap of a video for the song behind it changes the row's id and runtime, never
        // its artwork, so the covers are read without it.
        let tracks = self.api.playlist_page(playlist_id).await?.tracks;
        let tracks: Vec<Track> = tracks
            .into_iter()
            .enumerate()
            .map(|(index, track)| wire::track(track, index as u32))
            .collect();
        Ok(crate::distinct_covers(&tracks, wanted))
    }

    async fn track_radio(
        &self,
        track_id: &str,
        from: Option<&str>,
    ) -> Result<(Vec<Track>, Option<String>)> {
        radio::station(&self.api, &self.resolver, track_id, from).await
    }

    async fn search(&self, query: &str) -> Result<Vec<Track>> {
        let tracks = self
            .resolver
            .swap(self.api.search_songs(query).await?)
            .await;
        Ok(tracks
            .into_iter()
            .enumerate()
            .map(|(index, track)| wire::track(track, index as u32))
            .collect())
    }

    async fn search_albums(&self, query: &str) -> Result<Vec<Album>> {
        Ok(self
            .api
            .search_albums(query)
            .await?
            .into_iter()
            .map(wire::album)
            .collect())
    }

    async fn search_playlists(&self, query: &str) -> Result<Vec<Playlist>> {
        Ok(self
            .api
            .search_playlists(query)
            .await?
            .into_iter()
            .map(|playlist| wire::playlist(playlist, false, false))
            .collect())
    }

    async fn home(&self) -> Result<HomeFeed> {
        let mut feed = genres::home(
            self.api.clone(),
            self.resolver.clone(),
            self.account.clone(),
        );
        let mut whole = HomeFeed::default();
        while let Some(lot) = feed.recv().await {
            whole = lot?;
        }
        Ok(whole)
    }

    async fn home_paged(&self) -> Result<Feed> {
        Ok(genres::home(
            self.api.clone(),
            self.resolver.clone(),
            self.account.clone(),
        ))
    }

    async fn genres(&self) -> Result<Vec<Genre>> {
        genres::genres(&self.api).await
    }

    async fn genre(&self, genre_id: &str) -> Result<GenreDetail> {
        genres::genre(&self.api, &self.resolver, genre_id).await
    }

    /// The first cover on the category's own page, since the category buttons carry only a colour.
    async fn genre_cover(&self, genre_id: &str) -> Result<Option<String>> {
        genres::genre_cover(&self.api, genre_id).await
    }
}

fn collect_thumbnails(node: &serde_json::Value) -> Vec<ytmusic::Thumbnail> {
    node.get("thumbnails")
        .and_then(serde_json::Value::as_array)
        .map(|thumbs| {
            thumbs
                .iter()
                .filter_map(|thumb| {
                    Some(ytmusic::Thumbnail {
                        url: thumb.get("url")?.as_str()?.to_string(),
                        width: thumb.get("width").and_then(serde_json::Value::as_u64)? as u32,
                        height: thumb.get("height").and_then(serde_json::Value::as_u64)? as u32,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A playlist of the library as Sonora lists it: one the user made is theirs by name, and the
/// saved-episodes list YouTube keeps for podcasts is left out.
pub(crate) fn library_playlist(source: ytmusic::Playlist, account: &str) -> Option<Playlist> {
    if source.id == EPISODES {
        return None;
    }
    let mut playlist = wire::playlist(source, false, false);
    if playlist.owned && playlist.owner.is_empty() {
        playlist.owner = account.to_owned();
    }

    Some(playlist)
}
