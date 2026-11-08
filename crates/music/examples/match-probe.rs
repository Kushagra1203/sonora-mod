//! Shows why a video row's song is not found: the candidates a search brings back beside the
//! match the resolver would take.

use std::sync::Arc;

use anyhow::Result;
use ytmusic::YtMusic;

#[tokio::main]
async fn main() -> Result<()> {
    let jar = std::env::var("YTMUSIC_COOKIES").unwrap_or_else(|_| {
        "/home/kushagra/.local/share/sonora/accounts/youtube/cookies.json".into()
    });
    let api = Arc::new(YtMusic::with_cookies("").persist_cookies(jar.into()));

    for (video_id, title, artists) in [
        ("egOo6WvP1kE", "My Guy (Official Video)", "Sam Smith"),
        (
            "CLSuAhT71yU",
            "bloodstream (Official Music Video)",
            "Alyssa Grace",
        ),
    ] {
        println!("=== {video_id} — {title} — {artists}");
        let query = format!("{title} {artists}");
        let candidates = api.search_songs(&query).await?;
        println!("  candidates {}", candidates.len());
        for candidate in candidates.iter().take(10) {
            let names = candidate
                .artists
                .iter()
                .map(|artist| artist.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            let matched = ytmusic::dedup::best_song_match(
                &ytmusic::Track {
                    video_id: Some(video_id.to_owned()),
                    title: title.to_owned(),
                    artists: vec![ytmusic::ArtistRef {
                        name: artists.to_owned(),
                        id: None,
                    }],
                    kind: ytmusic::TrackKind::Video,
                    ..ytmusic::Track::default()
                },
                vec![candidate.clone()],
            );
            println!(
                "  {:14} kind={:?} dur={:?} match={:5} artists={:?} title={:?}",
                candidate.video_id.clone().unwrap_or_default(),
                candidate.kind,
                candidate.duration.map(|d| d.as_secs()),
                matched.is_some(),
                names,
                candidate.title
            );
        }
    }
    Ok(())
}
