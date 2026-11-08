//! Runs a video through the resolver the way the app does, so the song behind it — or the
//! reason there is none — can be seen.

use std::sync::Arc;

use anyhow::Result;
use music::youtube::resolve::Resolver;
use ytmusic::YtMusic;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();
    let jar = std::env::var("YTMUSIC_COOKIES").unwrap_or_else(|_| {
        "/home/kushagra/.local/share/sonora/accounts/youtube/cookies.json".into()
    });
    let api = Arc::new(YtMusic::with_cookies("").persist_cookies(jar.into()));
    let resolver = Arc::new(Resolver::new(api.clone(), music::youtube::resolve::path()));

    for id in std::env::args().skip(1) {
        // The row as the app would see it: what YouTube says about the video.
        let answer = api
            .execute(
                "next",
                ytmusic::Client::Music,
                serde_json::json!({ "videoId": id }),
            )
            .await?;
        let Some(row) = ytmusic::parse::find_renderers(&answer, "playlistPanelVideoRenderer")
            .into_iter()
            .next()
            .and_then(ytmusic::parse::panel_track)
        else {
            println!("=== {id}: no row");
            continue;
        };
        println!("=== {id}: {:?} — {:?}", row.title, row.artists);
        let song = resolver.one(row.clone()).await;
        println!(
            "    -> video={} title={:?} id={:?}",
            song.is_video(),
            song.title,
            song.video_id
        );
    }
    Ok(())
}
