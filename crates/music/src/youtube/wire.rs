use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use ytmusic::nav::Nav as _;
use ytmusic::parse;

use crate::models::{
    Album, AlbumDetail, Artist, ArtistProfile, ArtistRef, GenreItem, Playlist, PlaylistDetail,
    RelatedArtist, ReleaseType, Shelves, Track, UserProfile,
};
use crate::youtube::resolve::Resolver;

/// The longer edge of a large cover, wide enough for the fullscreen player on a 2x display.
const COVER_LARGE: u32 = 1440;
/// The longer edge of an artist banner, which spans the whole page.
const BANNER: u32 = 1440;

pub fn track(source: ytmusic::Track, index: u32) -> Track {
    let is_video = source.is_video();
    let artists = source
        .artists
        .iter()
        .map(|artist| artist.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let artist_refs = source
        .artists
        .into_iter()
        .map(|artist| ArtistRef {
            name: artist.name,
            id: artist.id,
        })
        .collect();
    Track {
        id: source.video_id,
        name: source.title,
        playable: source.available,
        is_video,
        artists,
        artist_refs,
        album: source
            .album
            .as_ref()
            .map(|album| album.name.clone())
            .unwrap_or_default(),
        album_id: source.album.and_then(|album| album.id),
        cover: cover(&source.thumbnails),
        duration: source.duration.unwrap_or(Duration::ZERO),
        added_at: None,
        added_by: None,
        playcount: None,
        popularity: 0,
        explicit: source.explicit,
        track_number: index + 1,
        disc_number: 1,
        tags: Vec::new(),
        languages: Vec::new(),
        credits: Vec::new(),
    }
}

/// Swaps every video in `tracks` for the song recording behind it. Only the playable id and
/// the runtime are taken from the song: the listing's title, credits, album and artwork are
/// what the queue should keep showing. A track that is already a song, or whose song cannot be
/// found with confidence, is left exactly as it is.
pub(crate) async fn audio_only(resolver: &Arc<Resolver>, tracks: Vec<Track>) -> Vec<Track> {
    if !tracks.iter().any(|track| track.is_video) {
        return tracks;
    }
    let sources: Vec<ytmusic::Track> = tracks.iter().map(source_of).collect();
    let swapped = resolver.swap(sources).await;
    tracks
        .into_iter()
        .zip(swapped)
        .map(|(track, song)| match song.is_video() {
            true => track,
            false => resolved(track, song),
        })
        .collect()
}

/// Takes the song's playable id and runtime onto the listing's row.
fn resolved(mut track: Track, song: ytmusic::Track) -> Track {
    if let Some(id) = song.video_id {
        track.id = Some(id);
    }
    if let Some(duration) = song.duration {
        track.duration = duration;
    }
    if track.album.trim().is_empty()
        && let Some(album) = song.album
    {
        track.album = album.name;
        track.album_id = album.id;
    }
    track.is_video = false;
    track
}

/// The same swap across a shelf's cards, so the songs survive being listed beside albums and
/// playlists.
pub(crate) async fn audio_only_items(
    resolver: &Arc<Resolver>,
    items: Vec<GenreItem>,
) -> Vec<GenreItem> {
    let mut seats = Vec::new();
    let mut tracks = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if let GenreItem::Track(track) = item {
            seats.push(index);
            tracks.push(track.clone());
        }
    }
    if tracks.is_empty() {
        return items;
    }
    let swapped = audio_only(resolver, tracks).await.into_iter();
    let mut items = items;
    for (index, track) in seats.into_iter().zip(swapped) {
        items[index] = GenreItem::Track(track);
    }
    items
}

/// The queue's view of a track as the resolver reads it: enough of the title, credits,
/// album and runtime for the search to find the same recording.
fn source_of(track: &Track) -> ytmusic::Track {
    let artists = match track.artist_refs.is_empty() {
        true => track
            .artists
            .split(", ")
            .filter(|name| !name.is_empty())
            .map(|name| ytmusic::ArtistRef {
                name: name.to_owned(),
                id: None,
            })
            .collect(),
        false => track
            .artist_refs
            .iter()
            .map(|artist| ytmusic::ArtistRef {
                name: artist.name.clone(),
                id: artist.id.clone(),
            })
            .collect(),
    };
    ytmusic::Track {
        video_id: track.id.clone(),
        title: track.name.clone(),
        artists,
        album: (!track.album.trim().is_empty()).then(|| ytmusic::AlbumRef {
            name: track.album.clone(),
            id: track.album_id.clone(),
        }),
        duration: (!track.duration.is_zero()).then_some(track.duration),
        thumbnails: Vec::new(),
        explicit: track.explicit,
        available: track.playable,
        kind: match track.is_video {
            true => ytmusic::TrackKind::Video,
            false => ytmusic::TrackKind::Song,
        },
        set_video_id: None,
        liked: None,
        views: None,
    }
}

pub fn album(source: ytmusic::Album) -> Album {
    let artists = source
        .artists
        .iter()
        .map(|artist| artist.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Album {
        id: source.browse_id,
        name: source.title,
        artists,
        artist_refs: source
            .artists
            .into_iter()
            .map(|artist| ArtistRef {
                name: artist.name,
                id: artist.id,
            })
            .collect(),
        cover: cover(&source.thumbnails),
        cover_large: cover_large(&source.thumbnails),
        release_type: release_type(source.kind),
        year: source.year.unwrap_or(0),
        track_count: source.track_count.unwrap_or(0),
        release_date: source.year.map(|year| year.to_string()).unwrap_or_default(),
        label: String::new(),
        copyrights: Vec::new(),
        added_at: None,
    }
}

pub fn album_detail(source: ytmusic::AlbumDetail) -> AlbumDetail {
    let tracks = source
        .tracks
        .into_iter()
        .enumerate()
        .map(|(index, item)| track(item, index as u32))
        .collect();
    AlbumDetail {
        album: album(source.album),
        tracks,
    }
}

pub fn playlist(source: ytmusic::Playlist, owned: bool, public: bool) -> Playlist {
    Playlist {
        id: source.id,
        name: source.title,
        owner: source.author.unwrap_or_default(),
        owner_id: String::new(),
        owned: owned || source.owned,
        collaborative: false,
        blend: false,
        public: source.public.unwrap_or(public),
        cover: cover(&source.thumbnails),
        track_count: source.track_count.unwrap_or(0),
        modified_at: None,
    }
}

pub fn playlist_detail(source: ytmusic::PlaylistDetail) -> PlaylistDetail {
    let owned = source.playlist.owned;
    let public = source.public;
    let tracks = source
        .tracks
        .into_iter()
        .enumerate()
        .map(|(index, item)| track(item, index as u32))
        .collect();
    PlaylistDetail {
        playlist: playlist(source.playlist, owned, public),
        tracks,
        continuation: source.continuation,
    }
}

pub fn playlist_page(source: ytmusic::PlaylistPage) -> (Vec<Track>, Option<String>) {
    let tracks = source
        .tracks
        .into_iter()
        .enumerate()
        .map(|(index, item)| track(item, index as u32))
        .collect();
    (tracks, source.continuation)
}

pub fn artist(source: ytmusic::Artist) -> Artist {
    let top_tracks = source
        .top_tracks
        .into_iter()
        .enumerate()
        .map(|(index, item)| track(item, index as u32))
        .collect();
    let albums = source
        .albums
        .into_iter()
        .chain(source.singles)
        .map(album)
        .collect();
    Artist {
        name: source.name,
        cover_large: cover_large(&source.thumbnails),
        banner: banner(&source.thumbnails),
        biography: source.description,
        monthly_listeners: None,
        subscribers: None,
        shelves: Shelves::default(),
        top_tracks,
        albums,
    }
}

/// Reads what an artist page carries beyond the releases and the top songs: the header's real
/// counts and the shelves under them, which are the playlists the artist is featured on, the
/// playlists it keeps and the artists close to it. The crate reads none of this, so it is read
/// straight off the page's own response. Anything the page does not carry is left as it was.
pub fn artist_page(artist: &mut Artist, page: &Value) {
    let header = parse::find_renderer(page, "musicImmersiveHeaderRenderer")
        .or_else(|| parse::find_renderer(page, "musicVisualHeaderRenderer"))
        .or_else(|| parse::find_renderer(page, "musicHeaderRenderer"));
    if let Some(header) = header {
        artist.monthly_listeners = count_of(header.run_text(&["monthlyListenerCount"]).as_deref());
        artist.subscribers = count_of(
            header
                .run_text(&[
                    "subscriptionButton",
                    "subscribeButtonRenderer",
                    "longSubscriberCountText",
                ])
                .as_deref(),
        );
    }

    for shelf in parse::find_renderers(page, "musicCarouselShelfRenderer") {
        let title = shelf
            .run_text(&["header", "musicCarouselShelfBasicHeaderRenderer", "title"])
            .unwrap_or_default();
        let items = shelf.items(&["contents"]);
        match title.as_str() {
            "Featured on" => {
                artist.shelves.featured_on = items
                    .iter()
                    .filter_map(parse::two_row_playlist)
                    .map(|source| playlist(source, false, true))
                    .collect();
            }
            // The shelf is titled for the artist it belongs to, so it is told apart by name.
            title if title.starts_with("Playlists by ") => {
                artist.shelves.artist = items
                    .iter()
                    .filter_map(parse::two_row_playlist)
                    .map(|source| playlist(source, false, true))
                    .collect();
            }
            "Fans might also like" => {
                artist.shelves.fans = items.iter().filter_map(related).collect();
            }
            _ => {}
        }
    }
}

/// One artist off a "fans also like" shelf, with the listeners its caption reports beside it.
fn related(item: &Value) -> Option<RelatedArtist> {
    let renderer = item.at(&["musicTwoRowItemRenderer"]).unwrap_or(item);
    let id = renderer
        .str_at(&["navigationEndpoint", "browseEndpoint", "browseId"])?
        .to_owned();

    Some(RelatedArtist {
        id,
        name: renderer.run_text(&["title"])?,
        cover: cover(&parse::thumbnails(renderer)),
        listeners: renderer
            .runs(&["subtitle"])
            .iter()
            .filter_map(|run| run.str_at(&["text"]))
            .find_map(|text| count_of(Some(text))),
    })
}

/// Where the "Show all" link under the top songs shelf leads, with the params the page asks for
/// alongside the id.
pub fn top_songs_endpoint(page: &Value) -> Option<(String, Option<String>)> {
    let shelf = parse::find_renderer(page, "musicShelfRenderer")?;
    let browse = shelf.at(&["bottomEndpoint", "browseEndpoint"])?;
    Some((
        browse.str_at(&["browseId"])?.to_owned(),
        browse.str_at(&["params"]).map(str::to_owned),
    ))
}

/// The songs on an artist's top songs page, in the order the page ranks them. They come back
/// as the crate read them so the caller can swap the videos for their songs before wiring.
pub fn top_songs(page: &Value) -> Vec<ytmusic::Track> {
    let mut tracks = Vec::new();
    for shelf in parse::find_renderers(page, "musicPlaylistShelfRenderer") {
        collect_songs(shelf.items(&["contents"]), &mut tracks);
    }
    tracks
}

fn collect_songs(items: &[Value], tracks: &mut Vec<ytmusic::Track>) {
    for item in items {
        let Some(source) = parse::list_item_track(item) else {
            continue;
        };
        tracks.push(source);
    }
}

/// Reads a count the way a page writes it, such as "288M monthly audience" or "1.2M
/// subscribers", scaling by the suffix. A count the format is not known for is simply left out.
fn count_of(text: Option<&str>) -> Option<u64> {
    let head = text?.split_whitespace().next()?.to_owned();
    let (digits, scale) = match head.chars().last()? {
        'K' => (&head[..head.len() - 1], 1_000_u64),
        'M' => (&head[..head.len() - 1], 1_000_000),
        'B' => (&head[..head.len() - 1], 1_000_000_000),
        _ => (head.as_str(), 1),
    };
    let value: f64 = digits.replace([',', '\u{a0}', ' '], "").parse().ok()?;
    Some((value * scale as f64).round() as u64)
}

pub fn artist_profile(source: &ytmusic::Artist) -> ArtistProfile {
    ArtistProfile {
        name: source.name.clone(),
        cover_large: cover_large(&source.thumbnails),
        biography: source.description.clone(),
    }
}

pub fn profile(source: ytmusic::Profile) -> UserProfile {
    UserProfile {
        avatar: source
            .thumbnails
            .last()
            .map(|thumbnail| thumbnail.url.clone()),
        id: source.email.unwrap_or_else(|| source.name.clone()),
        display_name: source.name,
    }
}

fn release_type(kind: ytmusic::AlbumKind) -> ReleaseType {
    match kind {
        ytmusic::AlbumKind::Album => ReleaseType::Album,
        ytmusic::AlbumKind::Single => ReleaseType::Single,
        ytmusic::AlbumKind::Ep => ReleaseType::Ep,
        ytmusic::AlbumKind::Compilation => ReleaseType::Compilation,
    }
}

pub fn cover(thumbnails: &[ytmusic::Thumbnail]) -> Option<String> {
    thumbnails.last().map(|thumb| thumb.url.clone())
}

/// The largest of `thumbnails`, however the api happened to order them.
fn widest(thumbnails: &[ytmusic::Thumbnail]) -> Option<&ytmusic::Thumbnail> {
    thumbnails
        .iter()
        .max_by_key(|thumb| thumb.width.max(thumb.height))
}

/// The largest thumbnail, asked of Google's image server at `COVER_LARGE` on its longer edge.
/// YouTube Music hands out 544 pixels at most, though the server scales the same picture to any
/// size.
pub fn cover_large(thumbnails: &[ytmusic::Thumbnail]) -> Option<String> {
    widest(thumbnails).map(|thumb| resized(&thumb.url, COVER_LARGE))
}

/// The artist header picture when it is the wide one an immersive header carries. A visual header
/// only has the square portrait, which makes no banner.
pub fn banner(thumbnails: &[ytmusic::Thumbnail]) -> Option<String> {
    let widest = widest(thumbnails)?;
    (widest.width > widest.height).then(|| resized(&widest.url, BANNER))
}

/// Rewrites the sizing options of a googleusercontent url so its longer edge is `edge`, keeping
/// the aspect and every other option. Handles both the `=w<width>-h<height>` form and the `=s<n>`
/// one, which the server reads as the longer edge of a square. Any other url comes back as it is.
fn resized(url: &str, edge: u32) -> String {
    let Some((base, options)) = url.rsplit_once('=') else {
        return url.to_owned();
    };
    if !base.contains(".googleusercontent.com/") {
        return url.to_owned();
    }
    let size = |prefix: char| {
        options
            .split('-')
            .find_map(|option| option.strip_prefix(prefix)?.parse::<u32>().ok())
    };
    let numeric = |option: &str, prefix: char| {
        option
            .strip_prefix(prefix)
            .is_some_and(|value| value.parse::<u32>().is_ok())
    };
    let (width, height) = match (size('w'), size('h')) {
        (Some(width), Some(height)) => (width, height),
        // A plain `=s<n>` url measures a single square edge instead.
        _ => match size('s').filter(|edge| *edge > 0) {
            Some(square) => (square, square),
            None => return url.to_owned(),
        },
    };
    let longer = width.max(height);
    if longer == 0 {
        return url.to_owned();
    }
    let scale = |side: u32| u64::from(side) * u64::from(edge) / u64::from(longer);
    let options = options
        .split('-')
        .map(|option| {
            if numeric(option, 'w') {
                format!("w{}", scale(width))
            } else if numeric(option, 'h') {
                format!("h{}", scale(height))
            } else if numeric(option, 's') {
                format!("s{}", scale(width))
            } else {
                option.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("-");
    format!("{base}={options}")
}
