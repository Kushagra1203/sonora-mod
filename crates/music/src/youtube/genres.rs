use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use serde_json::{Value, json};
use ytmusic::nav::Nav as _;
use ytmusic::{Client, YtMusic, parse};

use crate::youtube::client::library_playlist;
use crate::youtube::resolve::Resolver;
use crate::youtube::wire;
use crate::{Feed, Genre, GenreDetail, GenreItem, GenreSection, HomeFeed, SavedArtist, Track};

const HOME: &str = "FEmusic_home";
/// The account's own listening history, which Google hands over only to a request that names
/// the account it is made for.
const HISTORY: &str = "FEmusic_history";
/// The pages that stand in for YouTube's own picks when the home page carries none.
const NEW_RELEASES: &str = "FEmusic_new_releases";
const EXPLORE: &str = "FEmusic_explore";
const QUICK_PICKS: &str = "Quick picks";
const FROM_LIBRARY: &str = "From your library";
/// The shelves that open the page, in this order; every other shelf keeps YouTube's order
/// behind.
const LEADING: [&str; 2] = [FROM_LIBRARY, "Recaps"];
/// How many of the library's playlists stand in for `FROM_LIBRARY` until YouTube's own comes.
const STAND_IN: usize = 10;
/// A shelf of music videos: Sonora plays audio only, so it is left out.
const VIDEOS: &str = "Music videos for you";
/// A shelf of music videos on a genre's own page, which YouTube fills with whatever it holds
/// for the genre. Sonora plays audio only, so it is left out as well.
const GENRE_VIDEOS: &str = "Music videos";
const QUICK_PICKS_LIMIT: usize = 15;
/// How many of the account's listening history the page opens with: one row for each song,
/// however often it was played.
const RECENTS_LIMIT: usize = 20;
/// How many lots of shelves the home feed is followed for; the web client stops around here.
const PAGES: usize = 8;
const CATEGORIES: &str = "FEmusic_moods_and_genres";
const CATEGORY: &str = "FEmusic_moods_and_genres_category";
const THUMB: u32 = 120;
const ARTIST_PAGE: &str = "MUSIC_PAGE_TYPE_ARTIST";
const SEPARATOR: &str = " • ";

/// The home feed. YouTube hands it out a few shelves at a time behind a continuation, and
/// the web client asks for the next lot as the page scrolls, so this follows the chain until
/// it ends or `PAGES` is reached. Quick picks are lifted out wherever they turn up, every
/// other shelf keeps its place. A lot that fails ends the feed with what came before it.
///
/// The page opens with the recents and the picks, and the recents arrive beside the first
/// page: the account's own listening history, newest first. YouTube's "Listen again" shelf
/// ranks by how often a song was played rather than how recently, so it stays below, where
/// its ranking is a feature, and the history leads the page instead.
///
/// The first lot waits for the picks, so the top of the page never draws without the row the
/// listener came for; every lot after it carries everything gathered so far, so the page fills
/// below the fold as the shelves come. When YouTube sends no picks of its own, the pages that
/// stand in for them are asked for beside the feed's next lot, so the page waits for neither
/// of them longer than the other.
///
/// The songs behind the shelves' videos are resolved after the page is up: a search that is
/// slow, or that YouTube refuses for a moment, then costs the rows their audio version rather
/// than the whole page.
///
/// "From your library" is among the last shelves YouTube sends, yet it opens the page, so the
/// first lot carries a stand-in built from the library's own playlists, fetched beside the
/// first page; YouTube's shelf takes its place when it comes.
pub(crate) fn home(api: Arc<YtMusic>, resolver: Arc<Resolver>, account: String) -> Feed {
    let (sink, feed) = tokio::sync::mpsc::channel(PAGES + 1);
    tokio::spawn(async move {
        let (first, playlists, recents) = tokio::join!(
            wire::patience(|| api.execute("browse", Client::Music, json!({ "browseId": HOME }))),
            api.library_playlists(),
            recents(&api),
        );
        let mut answer = match first {
            Ok(answer) => answer,
            Err(error) => {
                sink.send(Err(error)).await.ok();
                return;
            }
        };
        let mut whole = HomeFeed::default();
        whole.recents = recents;
        match playlists {
            Ok(playlists) => whole.sections.extend(stand_in(playlists, &account)),
            Err(error) => log::warn!("youtube: cannot list the library playlists: {error:#}"),
        }
        let mut sent = false;
        for page in 1..=PAGES {
            gather(&answer, &mut whole);
            // The picks YouTube has of its own come with the home page. When it sends none,
            // the pages that stand in for them are asked for beside the feed's next lot, so
            // the page waits for neither of them longer than the other.
            let token = continuation(&answer).map(str::to_owned);
            let wanted = page == 1 && !has_picks(&whole);
            let (next, standing_in) = tokio::join!(
                async {
                    match token {
                        Some(token) => Some(
                            wire::patience(|| {
                                api.execute(
                                    "browse",
                                    Client::Music,
                                    json!({ "continuation": token }),
                                )
                            })
                            .await,
                        ),
                        None => None,
                    }
                },
                async {
                    match wanted {
                        true => standing_in(&api).await,
                        false => None,
                    }
                },
            );
            if let Some(picks) = standing_in {
                whole.quick_picks = Some(picks);
            }
            if opened(&whole) || page == PAGES {
                if sink.send(Ok(arranged(&whole))).await.is_err() {
                    return;
                }
                sent = true;
            }
            let Some(next) = next else {
                break;
            };
            answer = match next {
                Ok(next) => next,
                Err(error) => {
                    log::warn!("youtube: cannot load home page {}: {error:#}", page + 1);
                    break;
                }
            };
        }
        swap_shelf_tracks(&resolver, &mut whole).await;
        sink.send(Ok(arranged(&whole))).await.ok();
        if !sent {
            log::warn!("youtube: the home feed ended without quick picks");
        }
    });

    feed
}

/// The account's own listening history, newest first: the songs the listener played, one row
/// for each song however often it was played. A guest session has no history to read, and a
/// signed-in one whose history cannot be read has none to show either.
async fn recents(api: &Arc<YtMusic>) -> Vec<GenreItem> {
    if !api.is_authenticated() {
        return Vec::new();
    }
    let answer = match wire::patience(|| api.browse_account(HISTORY)).await {
        Ok(answer) => answer,
        Err(error) => {
            log::warn!("youtube: cannot read the listening history: {error:#}");
            return Vec::new();
        }
    };

    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for shelf in parse::find_renderers(&answer, "musicShelfRenderer") {
        for node in shelf.items(&["contents"]) {
            let Some(source) = parse::list_item_track(node) else {
                continue;
            };
            let Some(id) = source.video_id.clone() else {
                continue;
            };
            if !seen.insert(id) {
                continue;
            }
            rows.push(GenreItem::Track(wire::track(source, rows.len() as u32)));
            if rows.len() >= RECENTS_LIMIT {
                return rows;
            }
        }
    }
    rows
}

/// The picks that stand in for YouTube's own when the home page carries none: the songs of
/// its new releases and explore pages. The first page that holds songs answers, the other is
/// not asked for.
async fn standing_in(api: &Arc<YtMusic>) -> Option<Vec<Track>> {
    for browse in [NEW_RELEASES, EXPLORE] {
        let answer = match wire::patience(|| {
            api.execute("browse", Client::Music, json!({ "browseId": browse }))
        })
        .await
        {
            Ok(answer) => answer,
            Err(error) => {
                log::warn!("youtube: cannot read {browse}: {error:#}");
                continue;
            }
        };
        let songs = songs(&answer);
        if !songs.is_empty() {
            return Some(songs);
        }
    }
    None
}

/// The songs a browse page holds, in the order YouTube lists them: the rows that play,
/// whether they are listed as songs or as the videos behind them.
fn songs(answer: &Value) -> Vec<Track> {
    parse::find_renderers(answer, "musicCarouselShelfRenderer")
        .into_iter()
        .chain(parse::find_renderers(answer, "musicShelfRenderer"))
        .flat_map(|shelf| items(shelf))
        .filter_map(|item| match item {
            GenreItem::Track(track) => Some(track),
            _ => None,
        })
        .take(QUICK_PICKS_LIMIT)
        .collect()
}

/// Swaps the videos on the feed's shelves for their audio-only songs, so a quick pick or a
/// recent row never sends a music video to the queue.
async fn swap_shelf_tracks(resolver: &Arc<Resolver>, feed: &mut HomeFeed) {
    feed.recents = wire::audio_only_items(resolver, std::mem::take(&mut feed.recents)).await;
    if let Some(picks) = feed.quick_picks.take() {
        feed.quick_picks = Some(wire::audio_only(resolver, picks).await);
    }
    for section in &mut feed.sections {
        section.items = wire::audio_only_items(resolver, std::mem::take(&mut section.items)).await;
    }
}

/// Whether the page can open: the picks it leads with are there. The recents come beside the
/// first page, so they are there too by the time this is asked, and an account with no history
/// to show is not waited for.
fn opened(feed: &HomeFeed) -> bool {
    has_picks(feed)
}

/// Whether the feed carries picks of its own. A shelf that holds nothing is no picks, so the
/// ones that stand in for them are asked for.
fn has_picks(feed: &HomeFeed) -> bool {
    feed.quick_picks
        .as_ref()
        .is_some_and(|picks| !picks.is_empty())
}

fn gather(answer: &Value, feed: &mut HomeFeed) {
    for shelf in parse::find_renderers(answer, "musicCarouselShelfRenderer") {
        match shelf_title(shelf).as_deref() {
            Some(QUICK_PICKS) => {
                feed.quick_picks = Some(tracks(shelf).into_iter().take(QUICK_PICKS_LIMIT).collect())
            }
            Some(VIDEOS) => {}
            _ => {
                if let Some(section) = section(shelf) {
                    place(feed, section);
                }
            }
        }
    }
}

/// Adds a shelf to the feed, in place of one already there under the same title, so a
/// stand-in gives way to the shelf it stood in for.
fn place(feed: &mut HomeFeed, section: GenreSection) {
    match feed
        .sections
        .iter()
        .position(|there| there.title == section.title)
    {
        Some(at) => feed.sections[at] = section,
        None => feed.sections.push(section),
    }
}

/// The library's first playlists as the `FROM_LIBRARY` shelf, the way `playlists` lists them:
/// YouTube's own shelf is those and the saved albums, so this is close until it arrives.
fn stand_in(playlists: Vec<ytmusic::Playlist>, account: &str) -> Option<GenreSection> {
    let items: Vec<GenreItem> = playlists
        .into_iter()
        .filter_map(|playlist| library_playlist(playlist, account))
        .take(STAND_IN)
        .map(GenreItem::Playlist)
        .collect();

    (!items.is_empty()).then(|| GenreSection {
        title: FROM_LIBRARY.to_owned(),
        items,
    })
}

/// The feed with the `LEADING` shelves first, in their order, and every other shelf behind them
/// in the order YouTube sent it.
fn arranged(feed: &HomeFeed) -> HomeFeed {
    let mut sections = feed.sections.clone();
    let mut arranged = Vec::with_capacity(sections.len());
    for title in LEADING {
        if let Some(at) = sections.iter().position(|section| section.title == title) {
            arranged.push(sections.remove(at));
        }
    }
    arranged.append(&mut sections);

    HomeFeed {
        recents: feed.recents.clone(),
        quick_picks: feed.quick_picks.clone(),
        sections: arranged,
    }
}

pub(crate) async fn genres(api: &YtMusic) -> Result<Vec<Genre>> {
    let answer =
        wire::patience(|| api.execute("browse", Client::Music, json!({ "browseId": CATEGORIES })))
            .await?;

    Ok(parse::find_renderers(&answer, "gridRenderer")
        .into_iter()
        .flat_map(|grid| grid.items(&["items"]))
        .filter_map(card)
        .collect())
}

pub(crate) async fn genre(
    api: &Arc<YtMusic>,
    resolver: &Arc<Resolver>,
    params: &str,
) -> Result<GenreDetail> {
    let answer = wire::patience(|| {
        api.execute(
            "browse",
            Client::Music,
            json!({ "browseId": CATEGORY, "params": params }),
        )
    })
    .await?;

    let mut detail = GenreDetail {
        name: answer
            .run_text(&["header", "musicHeaderRenderer", "title"])
            .unwrap_or_default(),
        sections: parse::find_renderers(&answer, "musicCarouselShelfRenderer")
            .into_iter()
            // The videos of a genre are the uploads YouTube holds for it, which for a genre as
            // broad as R&B is a shelf of anything. They are left out, as the home page leaves out
            // its own.
            .filter(|shelf| shelf_title(shelf).as_deref() != Some(GENRE_VIDEOS))
            .filter_map(section)
            .collect(),
    };
    for section in &mut detail.sections {
        // The song behind a video is searched for after the page is up: a search is slow, and
        // the page should not wait for every row it holds.
        section.items = wire::audio_only_known(resolver, std::mem::take(&mut section.items));
        wire::audio_only_later(resolver, &section.items);
    }
    Ok(detail)
}

/// The picture a genre tile wears, found without the rest of the page.
///
/// The page is the one the genre opens, so the picture is the one it wears there: the first
/// album's cover on it, else the first playlist's. Only the covers are looked for, so the
/// shelves are never read into items and their songs are never resolved to their audio.
pub(crate) async fn genre_cover(api: &Arc<YtMusic>, params: &str) -> Result<Option<String>> {
    let answer = wire::patience(|| {
        api.execute(
            "browse",
            Client::Music,
            json!({ "browseId": CATEGORY, "params": params }),
        )
    })
    .await?;

    let mut playlist = None;
    for shelf in parse::find_renderers(&answer, "musicCarouselShelfRenderer") {
        for node in shelf.items(&["contents"]) {
            // A card is a playlist or an album, a playlist first, as the page reads them.
            if let Some(source) = parse::two_row_playlist(node) {
                if playlist.is_none() {
                    playlist = thumb(&source.thumbnails);
                }
                continue;
            }
            if let Some(source) = parse::two_row_album(node)
                && let Some(cover) = thumb(&source.thumbnails)
            {
                return Ok(Some(cover));
            }
        }
    }
    Ok(playlist)
}

fn section(shelf: &Value) -> Option<GenreSection> {
    let title = shelf_title(shelf).unwrap_or_default();
    let items = items(shelf);

    (!items.is_empty()).then_some(GenreSection { title, items })
}

/// The songs and videos of a shelf, for a row that plays rather than browses.
fn tracks(shelf: &Value) -> Vec<Track> {
    items(shelf)
        .into_iter()
        .filter_map(|item| match item {
            GenreItem::Track(track) => Some(track),
            _ => None,
        })
        .collect()
}

/// Every card of a shelf in the order YouTube lists them, whatever each one is.
fn items(shelf: &Value) -> Vec<GenreItem> {
    shelf
        .items(&["contents"])
        .iter()
        .enumerate()
        .filter_map(|(index, node)| item(node, index))
        .collect()
}

fn shelf_title(shelf: &Value) -> Option<String> {
    shelf.run_text(&["header", "musicCarouselShelfBasicHeaderRenderer", "title"])
}

/// What kind of page a browse card leads to.
fn page_type(renderer: &Value) -> Option<&str> {
    renderer.str_at(&[
        "navigationEndpoint",
        "browseEndpoint",
        "browseEndpointContextSupportedConfigs",
        "browseEndpointContextMusicConfig",
        "pageType",
    ])
}

fn track(item: &Value) -> Option<ytmusic::Track> {
    let renderer = item.at(&["musicTwoRowItemRenderer"])?;
    if matches!(
        page_type(renderer),
        Some("MUSIC_PAGE_TYPE_ALBUM" | "MUSIC_PAGE_TYPE_PLAYLIST")
    ) {
        return None;
    }

    let endpoint = renderer.at(&[
        "thumbnailOverlay",
        "musicItemThumbnailOverlayRenderer",
        "content",
        "musicPlayButtonRenderer",
        "playNavigationEndpoint",
        "watchEndpoint",
    ])?;
    let video_id = endpoint.str_at(&["videoId"])?.to_owned();
    let artists = card_artists(renderer.runs(&["subtitle"]));
    let kind = match endpoint.str_at(&[
        "watchEndpointMusicSupportedConfigs",
        "watchEndpointMusicConfig",
        "musicVideoType",
    ]) {
        Some("MUSIC_VIDEO_TYPE_ATV") => ytmusic::TrackKind::Song,
        _ => ytmusic::TrackKind::Video,
    };

    Some(ytmusic::Track {
        video_id: Some(video_id),
        title: renderer.run_text(&["title"])?,
        artists,
        album: None,
        duration: None,
        thumbnails: parse::thumbnails(renderer),
        explicit: parse::explicit(renderer),
        available: true,
        kind,
        set_video_id: None,
        liked: None,
        views: None,
    })
}

/// The token for the next lot of shelves, wherever this page keeps it: the first page under
/// its section list, a continuation under `continuationContents`.
fn continuation(answer: &Value) -> Option<&str> {
    parse::find_renderers(answer, "nextContinuationData")
        .into_iter()
        .find_map(|next| next.str_at(&["continuation"]))
}

/// A shelf card as the item it stands for. A browse card is told apart by where it leads, so a
/// playlist and an album are tried first; a song or a video is what is left with a play button.
fn item(node: &Value, index: usize) -> Option<GenreItem> {
    if let Some(source) = parse::two_row_playlist(node) {
        let thumb = thumb(&source.thumbnails);
        let mut playlist = wire::playlist(source, false, true);
        playlist.cover = thumb;
        if let Some(owner) = playlist_owner(node) {
            playlist.owner = owner;
        }
        return Some(GenreItem::Playlist(playlist));
    }
    if let Some(source) = parse::two_row_album(node) {
        let thumb = thumb(&source.thumbnails);
        let mut album = wire::album(source);
        album.cover = thumb;
        return Some(GenreItem::Album(album));
    }
    if let Some(artist) = artist(node) {
        return Some(GenreItem::Artist(artist));
    }

    let source = parse::list_item_track(node).or_else(|| track(node))?;
    Some(GenreItem::Track(wire::track(source, index as u32)))
}

/// The artists under a song or video card. A channel with a page comes as a link; one without,
/// which many videos have, is plain text, so the words before the first separator are taken
/// as the name when no link is there.
fn card_artists(runs: &[Value]) -> Vec<ytmusic::ArtistRef> {
    let linked = parse::artist_runs(runs);
    if !linked.is_empty() {
        return linked;
    }

    runs.iter()
        .filter_map(|run| run.str_at(&["text"]))
        .take_while(|text| *text != SEPARATOR)
        .filter(|text| !matches!(*text, ", " | " & ") && !text.trim().is_empty())
        .filter(|text| *text != "Song" && *text != "Video")
        .map(|text| ytmusic::ArtistRef {
            name: text.to_owned(),
            id: None,
        })
        .collect()
}

/// Who a shelf playlist is by, as YouTube words it under the card: "Made for ekipazh fx" on a
/// recap, the artists of a mix, "Auto playlist" on the liked songs. The kind and the song count
/// are dropped from the subtitle, since the card says those on its own.
fn playlist_owner(node: &Value) -> Option<String> {
    let renderer = node.at(&["musicTwoRowItemRenderer"])?;
    let subtitle: String = renderer
        .runs(&["subtitle"])
        .iter()
        .filter_map(|run| run.str_at(&["text"]))
        .collect();
    let owner: Vec<&str> = subtitle
        .split(SEPARATOR)
        .map(str::trim)
        .filter(|part| !part.is_empty() && *part != "Playlist" && !counted(part))
        .collect();

    (!owner.is_empty()).then(|| owner.join(SEPARATOR))
}

fn counted(part: &str) -> bool {
    ["song", "songs", "track", "tracks", "view", "views"]
        .iter()
        .any(|unit| part.ends_with(unit))
        && part.starts_with(|c: char| c.is_ascii_digit())
}

fn artist(node: &Value) -> Option<SavedArtist> {
    let renderer = node.at(&["musicTwoRowItemRenderer"])?;
    if page_type(renderer) != Some(ARTIST_PAGE) {
        return None;
    }
    let id = renderer.str_at(&["navigationEndpoint", "browseEndpoint", "browseId"])?;
    if !id.starts_with("UC") {
        return None;
    }

    Some(SavedArtist {
        id: id.to_owned(),
        name: renderer.run_text(&["title"])?,
        cover: wire::cover(&parse::thumbnails(renderer)),
        added_at: None,
    })
}

fn thumb(thumbnails: &[ytmusic::Thumbnail]) -> Option<String> {
    thumbnails
        .iter()
        .find(|thumb| thumb.width >= THUMB)
        .or_else(|| thumbnails.last())
        .map(|thumb| thumb.url.clone())
}

fn card(item: &Value) -> Option<Genre> {
    let button = item.get("musicNavigationButtonRenderer")?;

    Some(Genre {
        id: button
            .str_at(&["clickCommand", "browseEndpoint", "params"])?
            .to_owned(),
        name: button.run_text(&["buttonText"])?,
        cover: None,
    })
}
