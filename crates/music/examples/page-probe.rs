//! Times the calls the home and artist pages make, so a slow page can be pinned on one of them.

use std::sync::Arc;
use std::time::Instant;

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

    let start = Instant::now();
    let artist = client.artist("UCKIOM0Wi1qNV8HFLArK-uYQ").await?;
    println!(
        "artist page      {:>6.2?}  top songs {}",
        start.elapsed(),
        artist.top_tracks.len()
    );

    let start = Instant::now();
    let mut feed = client.home_paged().await?;
    let mut pages = 0;
    let mut rows = 0;
    while let Some(answer) = feed.recv().await {
        let page = answer?;
        pages += 1;
        rows += page
            .sections
            .iter()
            .map(|section| section.items.len())
            .sum::<usize>();
        println!(
            "home page {pages}     {:>6.2?}  sections {} rows {rows}",
            start.elapsed(),
            page.sections.len()
        );
    }
    println!(
        "home done       {:>6.2?}  pages {pages} rows {rows}",
        start.elapsed()
    );
    Ok(())
}
