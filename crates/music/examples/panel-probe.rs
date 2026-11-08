//! Compares a playlist's own rows with the rows `next` gives for the same playlist.
//!
//! `next` with `isAudioOnly` asks for the audio version of each row, so a playlist whose rows
//! are official videos comes back with the songs. One request replaces a search per row.

use std::sync::Arc;

use anyhow::Result;
use ytmusic::YtMusic;
use ytmusic::nav::Nav as _;

#[tokio::main]
async fn main() -> Result<()> {
    let jar = std::env::var("YTMUSIC_COOKIES").unwrap_or_else(|_| {
        "/home/kushagra/.local/share/sonora/accounts/youtube/cookies.json".into()
    });
    let api = Arc::new(YtMusic::with_cookies("").persist_cookies(jar.into()));

    for playlist_id in std::env::args().skip(1) {
        let browse = format!("VL{}", playlist_id.trim_start_matches("VL"));
        let Ok(answer) = api
            .execute(
                "browse",
                ytmusic::Client::Music,
                serde_json::json!({ "browseId": browse }),
            )
            .await
        else {
            println!("=== {playlist_id}: browse failed");
            continue;
        };
        let rows: Vec<_> =
            ytmusic::parse::find_renderers(&answer, "musicResponsiveListItemRenderer")
                .into_iter()
                .filter_map(ytmusic::parse::list_item_track)
                .collect();
        println!("=== {playlist_id}: browse rows {}", rows.len());
        for track in rows.iter().take(8) {
            println!(
                "  browse {:14} kind={:?} dur={:?} {}",
                track.video_id.clone().unwrap_or_default(),
                track.kind,
                track.duration.map(|d| d.as_secs()),
                track.title
            );
        }

        let Ok(queue) = api
            .execute(
                "next",
                ytmusic::Client::Music,
                serde_json::json!({ "playlistId": playlist_id, "isAudioOnly": true }),
            )
            .await
        else {
            println!("  next failed");
            continue;
        };
        let panel: Vec<_> = ytmusic::parse::find_renderers(&queue, "playlistPanelVideoRenderer")
            .into_iter()
            .collect();
        println!("  next rows {}", panel.len());
        for row in panel.iter().take(8) {
            let kind = row
                .str_at(&[
                    "navigationEndpoint",
                    "watchEndpoint",
                    "watchEndpointMusicSupportedConfigs",
                    "watchEndpointMusicConfig",
                    "musicVideoType",
                ])
                .unwrap_or("-");
            println!(
                "  next   {:14} kind={kind:26} len={:?} {}",
                row.str_at(&["videoId"]).unwrap_or("-"),
                row.run_text(&["lengthText"]),
                row.run_text(&["title"]).unwrap_or_default(),
            );
        }
    }
    Ok(())
}
