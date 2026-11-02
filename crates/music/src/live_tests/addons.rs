//! Live checks against the addon servers, driven by an environment list so no address — and
//! no token an address carries — lives in the tree.
//!
//! Run with:
//!
//! ```sh
//! SONORA_ADDON_URLS="https://a/manifest.json,https://b/" \
//!     cargo test -p music --lib live_tests::addons -- --ignored --nocapture
//! ```
//!
//! Each address is asked for its manifest, searched, and one hit resolved to a stream whose
//! first bytes are read. A dead or expired link fails its own address and leaves the rest of
//! the list running, which is exactly the behaviour a rotation morning needs.

use anyhow::{Context as _, Result};

use crate::addon::client::{AddonClient, Tier};

/// What every addon is expected to hold; a plainly popular track keeps the check honest.
const QUERY: &str = "Yo-Sea Someday";

/// How much of a resolved stream is read before the connection is dropped: enough to prove
/// bytes arrive without dragging a 24/192 FLAC through the test.
const PEEK: usize = 128 * 1024;

#[tokio::test]
#[ignore = "reaches the addon servers named in SONORA_ADDON_URLS"]
async fn addons_answer_search_and_resolve_a_playable_stream() -> Result<()> {
    let Some(list) = std::env::var_os("SONORA_ADDON_URLS") else {
        println!("SONORA_ADDON_URLS is not set; nothing to check");
        return Ok(());
    };
    let mut failures: Vec<String> = Vec::new();

    for url in list
        .to_string_lossy()
        .split(',')
        .map(str::trim)
        .filter(|url| !url.is_empty())
    {
        if let Err(error) = check(url).await {
            println!("✗ {url}: {error:#}");
            failures.push(format!("{url}: {error:#}"));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of the addon checks failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
    Ok(())
}

/// One address, end to end: manifest, search, resolve, first bytes.
async fn check(url: &str) -> Result<()> {
    let client = AddonClient::new(url).context("the address does not parse")?;
    let manifest = client.manifest().await.context("no manifest")?;
    println!(
        "• {} v{} (search{} stream)",
        manifest.display_name(),
        manifest.version,
        match manifest.resources.iter().any(|r| r == "search") {
            true => "",
            false => "?",
        }
    );

    let rows = client.search(QUERY, Tier::Lossless).await?;
    println!("  search: {} rows", rows.len());
    let Some(row) = rows.first() else {
        anyhow::bail!("search answered nothing");
    };

    let Some(stream) = client.stream(&row.id, Tier::Lossless).await? else {
        anyhow::bail!("the first hit resolved to a miss");
    };
    anyhow::ensure!(!stream.is_encrypted(), "the stream is DRM-protected");
    println!(
        "  stream: format={:?} transport={:?} codec={:?} kbps={:?}",
        stream.format,
        stream.transport(),
        stream.stated_codec(),
        stream.kbps()
    );

    let mut response = reqwest::Client::new()
        .get(&stream.url)
        .send()
        .await
        .context("the stream url did not answer")?;
    anyhow::ensure!(
        response.status().is_success(),
        "the stream url answered HTTP {}",
        response.status()
    );
    let mut got = 0usize;
    while let Some(chunk) = response
        .chunk()
        .await
        .context("reading the stream failed")?
    {
        got += chunk.len();
        if got >= PEEK {
            break;
        }
    }
    anyhow::ensure!(got > 0, "the stream url answered no bytes");
    println!("  bytes: {got} read, then dropped");
    Ok(())
}
