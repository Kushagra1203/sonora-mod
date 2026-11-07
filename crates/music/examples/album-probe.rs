//! Looks at the watch queue YouTube Music builds for an album or a track, to see whether it
//! carries the audio-only version of every row.

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use ytmusic::nav::Nav as _;
use ytmusic::{Client, YtMusic};

#[tokio::main]
async fn main() -> Result<()> {
    let jar = std::env::var("YTMUSIC_COOKIES").unwrap_or_else(|_| {
        "/home/kushagra/.local/share/sonora/accounts/youtube/cookies.json".into()
    });
    let api = Arc::new(YtMusic::with_cookies("").persist_cookies(jar.into()));

    let mut body = serde_json::json!({ "isAudioOnly": true });
    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "album" => {
            let id = std::env::args().nth(2).expect("album id");
            body["playlistId"] = serde_json::json!(id);
            body["videoId"] = serde_json::json!("gsZauX8gwxo");
        }
        "track" => {
            let id = std::env::args().nth(2).expect("video id");
            body["videoId"] = serde_json::json!(id);
            body["playlistId"] = serde_json::json!(format!("RDAMVM{id}"));
        }
        other => anyhow::bail!("mode: album|track, not {other}"),
    }

    let start = Instant::now();
    let answer = api.execute("next", Client::Music, body).await?;
    println!("next in {:?}", start.elapsed());

    let mut wrappers = Vec::new();
    ytmusic::nav::find_all(&answer, "playlistPanelVideoWrapperRenderer", &mut wrappers);
    let mut rows = Vec::new();
    ytmusic::nav::find_all(&answer, "playlistPanelVideoRenderer", &mut rows);
    println!("wrappers {} rows {}", wrappers.len(), rows.len());

    for row in rows.iter().take(12) {
        let video_id = row.str_at(&["videoId"]);
        let kind = row.str_at(&[
            "navigationEndpoint",
            "watchEndpoint",
            "watchEndpointMusicSupportedConfigs",
            "watchEndpointMusicConfig",
            "musicVideoType",
        ]);
        let title = row.run_text(&["title"]);
        let length = row.run_text(&["lengthText"]);
        println!("  row {video_id:?} kind={kind:?} len={length:?} title={title:?}");
    }
    Ok(())
}
