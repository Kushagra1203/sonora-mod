//! Checks a station: seeded by a video it answers with videos, seeded by the song behind it
//! it answers with songs, so one lookup for the seed turns the whole queue into audio.

use std::sync::Arc;

use anyhow::Result;
use music::MusicApi;
use music::youtube::YouTubeClient;
use ytmusic::YtMusic;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let jar = std::env::var("YTMUSIC_COOKIES").unwrap_or_else(|_| {
        "/home/kushagra/.local/share/sonora/accounts/youtube/cookies.json".into()
    });
    let api = Arc::new(YtMusic::with_cookies("").persist_cookies(jar.into()));
    let client = YouTubeClient::new(api);

    for video_id in std::env::args().skip(1) {
        let (tracks, next) = client.track_radio(&video_id, None).await?;
        let videos = tracks.iter().filter(|track| track.is_video).count();
        println!(
            "=== station of {video_id}: {} rows, {videos} videos",
            tracks.len()
        );
        for track in tracks.iter().take(8) {
            println!(
                "  {:14} video={:5} dur={:>4}s {}",
                track.id.as_deref().unwrap_or("-"),
                track.is_video,
                track.duration.as_secs(),
                track.name,
            );
        }
        println!("  continuation: {}", next.unwrap_or_else(|| "-".into()));
    }
    Ok(())
}
