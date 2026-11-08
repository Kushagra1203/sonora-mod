//! Counts the videos an artist page's popular list holds, and the songs behind them.

use std::sync::Arc;

use anyhow::Result;
use music::MusicApi;
use music::youtube::YouTubeClient;
use ytmusic::YtMusic;

#[tokio::main]
async fn main() -> Result<()> {
    let jar = std::env::var("YTMUSIC_COOKIES").unwrap_or_else(|_| {
        "/home/kushagra/.local/share/sonora/accounts/youtube/cookies.json".into()
    });
    let api = Arc::new(YtMusic::with_cookies("").persist_cookies(jar.into()));
    let client = YouTubeClient::new(api);

    for artist_id in std::env::args().skip(1) {
        let artist = client.artist(&artist_id).await?;
        let videos = artist
            .top_tracks
            .iter()
            .filter(|track| track.is_video)
            .count();
        println!(
            "=== {} — {} top songs, {videos} videos",
            artist.name, artist_id
        );
        for track in artist.top_tracks.iter().take(12) {
            println!(
                "  {:14} video={:5} dur={:>4}s {}",
                track.id.as_deref().unwrap_or("-"),
                track.is_video,
                track.duration.as_secs(),
                track.name,
            );
        }
    }
    Ok(())
}
