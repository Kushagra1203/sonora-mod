//! Prints the artwork URLs an artist page hands back, and what each one actually measures when
//! the image server is asked for a larger edge. The hero on an artist page is only as sharp as
//! the picture behind it, so this is where the ceiling is measured.

use std::sync::Arc;

use anyhow::Result;
use music::MusicApi;
use music::youtube::YouTubeClient;
use ytmusic::YtMusic;

async fn measure(label: &str, url: &str) {
    let client = reqwest::Client::new();
    match client.get(url).send().await {
        Ok(response) => {
            let status = response.status();
            let bytes = response.bytes().await.unwrap_or_default();
            let size = image::load_from_memory(&bytes)
                .map(|image| format!("{}x{}", image.width(), image.height()))
                .unwrap_or_else(|_| "not an image".to_owned());
            println!("  {label:<28} {status} {} KiB  {size}", bytes.len() / 1024);
        }
        Err(error) => println!("  {label:<28} error {error}"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let jar = std::env::var("YTMUSIC_COOKIES").unwrap_or_else(|_| {
        "/home/kushagra/.local/share/sonora/accounts/youtube/cookies.json".into()
    });
    let api = Arc::new(YtMusic::with_cookies("").persist_cookies(jar.into()));
    let client = YouTubeClient::new(api);

    let query = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "expectations Olivia Rodrigo".to_owned());
    let tracks = client.search(&query).await?;
    let Some(track) = tracks.first() else {
        println!("no track for {query:?}");
        return Ok(());
    };
    let Some(artist_id) = track.artist_refs.iter().find_map(|a| a.id.clone()) else {
        println!("no artist id on {} — {}", track.name, track.artists);
        return Ok(());
    };

    let artist = client.artist(&artist_id).await?;
    println!("=== {} ({artist_id})", artist.name);
    println!("  cover  {}", artist.cover_large.as_deref().unwrap_or("-"));
    println!("  banner {}", artist.banner.as_deref().unwrap_or("-"));

    if let Some(url) = &artist.banner {
        let base = url.rsplit_once('=').map(|(b, _)| b).unwrap_or(url);
        println!("banner as served:");
        measure("as handed back", url).await;
        for edge in [1440_u32, 2560, 3840] {
            let asked = format!("{base}=w{edge}-h{}", edge * 9 / 16);
            measure(&format!("asked w{edge}"), &asked).await;
        }
        println!("banner square (no aspect):");
        for edge in [1440_u32, 2560] {
            measure(&format!("asked s{edge}"), &format!("{base}=s{edge}")).await;
        }
    }

    if let Some(url) = &artist.cover_large {
        let base = url.rsplit_once('=').map(|(b, _)| b).unwrap_or(url);
        println!("cover as served:");
        measure("as handed back", url).await;
        for edge in [2560_u32, 3840] {
            measure(
                &format!("asked w{edge}"),
                &format!("{base}=w{edge}-h{edge}"),
            )
            .await;
        }
    }
    Ok(())
}
