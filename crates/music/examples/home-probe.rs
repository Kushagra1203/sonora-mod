//! Reads the home feed as the app does, printing what each lot carries.

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use music::MusicApi;
use music::youtube::YouTubeClient;
use ytmusic::YtMusic;

const LEADING: [&str; 2] = ["From your library", "Recaps"];

#[tokio::main]
async fn main() -> Result<()> {
    let jar = std::env::var("YTMUSIC_COOKIES").unwrap_or_else(|_| {
        "/home/kushagra/.local/share/sonora/accounts/youtube/cookies.json".into()
    });
    let api = Arc::new(YtMusic::with_cookies("").persist_cookies(jar.into()));
    let client = YouTubeClient::new(api);

    let start = Instant::now();
    let mut feed = client.home_paged().await?;
    let mut lot = 0;
    while let Some(answer) = feed.recv().await {
        let feed = answer?;
        lot += 1;
        let titles: Vec<&str> = feed.sections.iter().map(|s| s.title.as_str()).collect();
        let opened = !feed.listen_again.is_empty()
            && feed.quick_picks.is_some()
            && LEADING.iter().all(|title| titles.contains(title));
        println!(
            "lot {lot} at {:>6.2?}  listen_again {} quick_picks {} sections {:?} opened {opened}",
            start.elapsed(),
            feed.listen_again.len(),
            feed.quick_picks
                .as_ref()
                .map(|picks| picks.len())
                .unwrap_or(0),
            titles,
        );
    }
    println!("done at {:?}", start.elapsed());
    Ok(())
}
