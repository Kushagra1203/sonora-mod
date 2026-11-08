//! Reads a video's timed captions, the way the web player does: the player response names the
//! caption track, and the track's own address carries the timings.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use ytmusic::YtMusic;
use ytmusic::nav::Nav as _;

#[tokio::main]
async fn main() -> Result<()> {
    let jar = std::env::var("YTMUSIC_COOKIES").unwrap_or_else(|_| {
        "/home/kushagra/.local/share/sonora/accounts/youtube/cookies.json".into()
    });
    let api = Arc::new(YtMusic::with_cookies("").persist_cookies(jar.into()));
    let http = reqwest::Client::new();

    for video_id in std::env::args().skip(1) {
        let answer = api
            .execute(
                "player",
                ytmusic::Client::Web,
                serde_json::json!({
                    "videoId": video_id,
                    "contentCheckOk": true,
                    "racyCheckOk": true,
                }),
            )
            .await?;
        let playability = answer
            .str_at(&["playabilityStatus", "status"])
            .unwrap_or("?");
        let tracks: Vec<_> = answer
            .items(&[
                "captions",
                "playerCaptionsTracklistRenderer",
                "captionTracks",
            ])
            .to_vec();
        println!(
            "=== {video_id}: playability {playability}, {} caption tracks",
            tracks.len()
        );
        for track in tracks.iter().take(4) {
            let language = track.str_at(&["languageCode"]).unwrap_or("-");
            let kind = track.str_at(&["kind"]).unwrap_or("manual");
            let Some(url) = track.str_at(&["baseUrl"]) else {
                continue;
            };
            println!("  {language} ({kind})");
            let url = format!("{url}&fmt=json3");
            let Ok(body) = http.get(&url).send().await else {
                println!("    no answer");
                continue;
            };
            let Ok(text) = body.text().await else {
                continue;
            };
            let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else {
                println!("    not json ({} bytes)", text.len());
                continue;
            };
            let mut lines = Vec::new();
            for event in parsed.items(&["events"]) {
                let Some(start) = event.at(&["tStartMs"]).and_then(|value| value.as_u64()) else {
                    continue;
                };
                let text: String = event
                    .items(&["segs"])
                    .iter()
                    .filter_map(|seg| seg.str_at(&["utf8"]))
                    .collect();
                let text = text.trim().to_owned();
                if text.is_empty() {
                    continue;
                }
                lines.push((Duration::from_millis(start), text));
            }
            println!("    {} timed lines", lines.len());
            for (start, text) in lines.iter().take(6) {
                println!("      {start:?}  {text}");
            }
        }
    }
    Ok(())
}
