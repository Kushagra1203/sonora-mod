//! Checks the play-time rule: a video row whose real runtime is the one its listing promised is
//! left alone, and one that really runs longer plays the song behind it.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use music::youtube::YouTubeClient;
use music::{MusicApi, Track};
use ytmusic::YtMusic;

#[tokio::main]
async fn main() -> Result<()> {
    let jar = std::env::var("YTMUSIC_COOKIES").unwrap_or_else(|_| {
        "/home/kushagra/.local/share/sonora/accounts/youtube/cookies.json".into()
    });
    let api = Arc::new(YtMusic::with_cookies("").persist_cookies(jar.into()));
    let client = YouTubeClient::new(api);

    // A video the listing promised at the song's runtime while it really runs longer: the album
    // row that played 4:52 out of a listing that said 3:44.
    let under = row(
        "gsZauX8gwxo",
        "expectations (feat. Chappell Roan) (Official Music Video)",
        "Olivia Rodrigo, Chappell Roan",
        Duration::from_secs(224),
    );
    // The official music video the "Pop Certified" chart lists: the listing promised the video's
    // own 4:10, so there is nothing hidden in it and it plays as it is.
    let honest = row(
        "q3zqJs7JUCQ",
        "Fortnight (feat. Post Malone)",
        "Taylor Swift",
        Duration::from_secs(250),
    );

    let visualizer = row(
        "s5EXFOaBTo0",
        "oh yeah? (Visualizer)",
        "Steve Lacy",
        Duration::from_secs(171),
    );
    let audio = row(
        "nO-h2erKi0A",
        "SLEEP (Official Audio)",
        "The Kid LAROI",
        Duration::from_secs(152),
    );
    let omv = row(
        "yT_AfrHbAyA",
        "THAT GIRL (Official Music Video)",
        "Tyla",
        Duration::from_secs(177),
    );

    for (label, track) in [
        ("listing under-promised", under),
        ("listing honest", honest),
        ("visualizer", visualizer),
        ("official audio", audio),
        ("omv", omv),
    ] {
        let played = client.song_behind(track.clone()).await?;
        println!(
            "{label:22} {:12} {:>4}s -> {:12} {:>4}s  {}",
            track.id.as_deref().unwrap_or("-"),
            track.duration.as_secs(),
            played.id.as_deref().unwrap_or("-"),
            played.duration.as_secs(),
            match played.is_video {
                true => "left as a video".to_owned(),
                false => format!("song: {}", played.name),
            }
        );
    }
    Ok(())
}

fn row(id: &str, name: &str, artists: &str, duration: Duration) -> Track {
    Track {
        id: Some(id.to_owned()),
        name: name.to_owned(),
        is_video: true,
        artists: artists.to_owned(),
        playable: true,
        artist_refs: Vec::new(),
        album: String::new(),
        album_id: None,
        cover: None,
        duration,
        added_at: None,
        added_by: None,
        playcount: None,
        popularity: 0,
        explicit: false,
        track_number: 1,
        disc_number: 1,
        tags: Vec::new(),
        languages: Vec::new(),
        credits: Vec::new(),
    }
}
