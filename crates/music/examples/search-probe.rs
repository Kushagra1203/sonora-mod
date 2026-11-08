//! Searches a list of queries the way the resolver does, so the refusals YouTube answers can be
//! seen side by side.

use std::sync::Arc;

use anyhow::Result;
use ytmusic::YtMusic;

#[tokio::main]
async fn main() -> Result<()> {
    let jar = std::env::var("YTMUSIC_COOKIES").unwrap_or_else(|_| {
        "/home/kushagra/.local/share/sonora/accounts/youtube/cookies.json".into()
    });
    let api = Arc::new(YtMusic::with_cookies("").persist_cookies(jar.into()));

    for query in std::env::args().skip(1) {
        match api.search_songs(&query).await {
            Ok(tracks) => println!("{query:?} -> {} songs", tracks.len()),
            Err(error) => println!("{query:?} -> REFUSED: {error}"),
        }
    }
    Ok(())
}
