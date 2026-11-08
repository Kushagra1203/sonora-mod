//! Checks the album route: an album's own page lists the videos it has for its songs, while the
//! backing playlist it names lists the songs themselves. One request for the playlist replaces
//! a search for every video row.

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

    for browse_id in std::env::args().skip(1) {
        let album = client.album(&browse_id).await?;
        println!("=== {} — {}", album.album.name, browse_id);
        for track in album.tracks.iter().take(10) {
            println!(
                "  {:14} video={:5} dur={:>4}s {:22} {}",
                track.id.as_deref().unwrap_or("-"),
                track.is_video,
                track.duration.as_secs(),
                track.artists.chars().take(22).collect::<String>(),
                track.name,
            );
        }
        let videos = album.tracks.iter().filter(|track| track.is_video).count();
        println!("  {} rows, {videos} videos", album.tracks.len());
    }
    Ok(())
}
