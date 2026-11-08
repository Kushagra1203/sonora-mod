//! Reads a video's timed captions through the provider the app uses, and the plain lyrics
//! YouTube Music holds for the same video, so the two can be compared.

use std::time::Duration;

use anyhow::Result;
use music::{
    Lyrics, LyricsProvider, LyricsQuery, TrackKey,
    youtube::{YouTubeLyrics, YouTubeTranscript},
};

#[tokio::main]
async fn main() -> Result<()> {
    let provider = YouTubeTranscript::new();
    let tab = YouTubeLyrics::new();
    for video_id in std::env::args().skip(1) {
        let query = LyricsQuery {
            title: "expectations".to_owned(),
            artist: "Olivia Rodrigo".to_owned(),
            album: None,
            duration: Duration::from_secs(222),
            track: Some(TrackKey {
                provider: "youtube",
                id: video_id.clone(),
            }),
        };
        if let Some(Lyrics::Plain { text, .. }) = tab.sheet(&video_id).await? {
            println!("=== {video_id}: lyrics tab says");
            for line in text.lines() {
                println!("    {line}");
            }
        } else {
            println!("=== {video_id}: lyrics tab is empty");
        }
        let hits = provider.search(&query).await?;
        println!("=== {video_id}: {} hits", hits.len());
        for hit in hits {
            println!(
                "  source {} trust {} synced {} worded {}",
                hit.source,
                hit.trust,
                hit.lyrics.synced(),
                hit.lyrics.worded()
            );
            let Lyrics::Synced { lines } = &hit.lyrics else {
                continue;
            };
            println!("  {} lines", lines.len());
            for line in lines.iter() {
                println!("    {:?}  {}", line.start, line.text);
            }
        }
    }
    Ok(())
}
