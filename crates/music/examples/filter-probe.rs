//! Searches one query in several ways, so the way YouTube answers it can be compared.
//!
//! The songs filter is what the resolver asks for. A query YouTube refuses under that filter
//! may still answer under another one, and the answer is worth as much as the filtered one.

use std::sync::Arc;

use anyhow::Result;
use ytmusic::YtMusic;

/// The filter the resolver's search goes out with.
const SONGS: &str = "EgWKAQIIAQ%3D%3D";
/// The filter a plain search goes out with.
const NONE: Option<&str> = None;

#[tokio::main]
async fn main() -> Result<()> {
    let jar = std::env::var("YTMUSIC_COOKIES").unwrap_or_else(|_| {
        "/home/kushagra/.local/share/sonora/accounts/youtube/cookies.json".into()
    });
    let api = Arc::new(YtMusic::with_cookies("").persist_cookies(jar.into()));

    for query in std::env::args().skip(1) {
        for (label, params) in [("songs", Some(SONGS)), ("plain", NONE)] {
            let mut payload = serde_json::json!({ "query": query });
            if let Some(params) = params {
                payload["params"] = serde_json::json!(params);
            }
            match api.execute("search", ytmusic::Client::Music, payload).await {
                Ok(answer) => {
                    let rows: Vec<_> =
                        ytmusic::parse::find_renderers(&answer, "musicResponsiveListItemRenderer")
                            .into_iter()
                            .filter_map(ytmusic::parse::list_item_track)
                            .collect();
                    let songs = rows.iter().filter(|row| !row.is_video()).count();
                    println!("{query:?} [{label}] -> {} rows, {songs} songs", rows.len());
                    for row in rows.iter().take(4) {
                        println!(
                            "    {:14} kind={:?} dur={:?} {}",
                            row.video_id.clone().unwrap_or_default(),
                            row.kind,
                            row.duration.map(|d| d.as_secs()),
                            row.title
                        );
                    }
                }
                Err(error) => println!("{query:?} [{label}] -> REFUSED: {error}"),
            }
        }
    }
    Ok(())
}
