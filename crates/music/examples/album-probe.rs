//! Reads an album through the client the app uses, so the resolver can be checked on the rows
//! it hands the queue.

use std::sync::Arc;

use anyhow::Result;
use music::youtube::YouTubeClient;
use music::MusicApi;
use ytmusic::YtMusic;

#[tokio::main]
async fn main() -> Result<()> {
    let jar = std::env::var("YTMUSIC_COOKIES")
        .unwrap_or_else(|_| "/home/kushagra/.local/share/sonora/accounts/youtube/cookies.json".into());
    let api = Arc::new(YtMusic::with_cookies("").persist_cookies(jar.into()));
    let client = YouTubeClient::new(api);
    for album_id in std::env::args().skip(1) {
        println!("--- {album_id}");
        let album = client.album(&album_id).await?;
        for track in album.tracks {
            println!(
                "{:12} {:>8}  {}",
                track.id.unwrap_or_default(),
                track.duration.as_secs(),
                track.name
            );
        }
    }
    Ok(())
}
