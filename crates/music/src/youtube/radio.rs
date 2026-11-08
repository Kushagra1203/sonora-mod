//! A track's station, in the order YouTube ranks it and as far as YouTube goes on serving it.
//! `ytmusic::track_radio` asks for the first panel alone, and the continuation that keeps a
//! station running past it is only in the raw answer, so the request is made here.

use std::sync::Arc;

use anyhow::Result;
use serde_json::{Value, json};
use ytmusic::nav::Nav as _;
use ytmusic::{Client, YtMusic, parse};

use crate::Track;
use crate::youtube::resolve::Resolver;
use crate::youtube::wire;

/// The station YouTube Music opens when a track is played on its own: the seed first, then what
/// YouTube's automix picks after it. With `from` it is the next stretch instead, which arrives
/// under a different renderer key. The seed goes with the continuation either way, since on its
/// own the token answers with most of the panel before it again.
///
/// A station seeded by a video answers with videos, every one of which would then need a search
/// for the song behind it. A station seeded by the song itself answers with songs, so the seed
/// is looked up once and the whole queue comes back as audio.
pub(crate) async fn station(
    api: &Arc<YtMusic>,
    resolver: &Arc<Resolver>,
    video_id: &str,
    from: Option<&str>,
) -> Result<(Vec<Track>, Option<String>)> {
    let key = match from {
        Some(_) => "playlistPanelContinuation",
        None => "playlistPanelRenderer",
    };
    // The seed of a station that has already started is the row's own id, which the queue
    // carries; only the first panel has to look the song up.
    let seed = match from {
        Some(_) => video_id.to_owned(),
        None => resolver.song_id(video_id).await,
    };
    let response =
        wire::patience(|| api.execute("next", Client::Music, request(&seed, from))).await?;
    let (tracks, next) = panel(&response, key);
    // A station seeded by a video answers with videos, so the songs behind them are what the
    // queue gets. A seed that is already a song never reaches the resolver.
    let tracks = resolver.swap(tracks).await;
    Ok((
        tracks
            .into_iter()
            .enumerate()
            .map(|(index, track)| wire::track(track, index as u32))
            .collect(),
        next,
    ))
}

/// The `next` request for the station seeded by `video_id`, at its start or at a continuation.
fn request(video_id: &str, continuation: Option<&str>) -> Value {
    let mut body = json!({
        "videoId": video_id,
        "playlistId": format!("RDAMVM{video_id}"),
        "enablePersistentPlaylistPanel": true,
        "tunerSettingValue": "AUTOMIX_SETTING_NORMAL",
    });
    if let Some(continuation) = continuation {
        body["continuation"] = json!(continuation);
    }
    body
}

/// The tracks of the first panel under `key`, and the continuation that follows them. A panel
/// with no radio continuation is the end of the station.
fn panel(response: &Value, key: &str) -> (Vec<ytmusic::Track>, Option<String>) {
    let Some(panel) = parse::find_renderers(response, key).into_iter().next() else {
        return (Vec::new(), None);
    };
    let tracks = panel
        .items(&["contents"])
        .iter()
        .filter_map(parse::panel_track)
        .collect();
    let next = panel
        .items(&["continuations"])
        .iter()
        .find_map(|item| item.str_at(&["nextRadioContinuationData", "continuation"]))
        .map(str::to_string);
    (tracks, next)
}
