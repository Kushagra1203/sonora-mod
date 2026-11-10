//! The artist's own signature, drawn in place of the typed name on their page.
//!
//! Autographs are kept by a small service that checks each one against the artist it is
//! credited to, so the page asks it by name and falls back to the typed name when it has
//! nothing. A signature is a few kilobytes of SVG, and it is held with the page rather than
//! written to disk.

use std::sync::Arc;

/// Where a signature is looked up: the artist's name is a path segment, and `/signatures` ends
/// the address.
const ENDPOINT: &str = "https://yellow-king-f562.chandu3319r.workers.dev/v1/artist";
/// The agent the service is asked with, the same one the other services here are asked with, so
/// a request that goes wrong is recognisable in a log.
const AGENT: &str = concat!(
    "sonora/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/sonorahq/sonora)"
);
/// How wide a signature is guessed to be when the service does not say. Most autographs are
/// wider than they are tall, and a guess is only used when the answer is missing a measurement.
const GUESSED_ASPECT: f32 = 3.;
/// The shapes a signature is allowed to be drawn in. Past these it is either a very short
/// monogram or a very long flourish, and either one would be drawn wrong by a guess.
const ASPECT_RANGE: std::ops::Range<f32> = 0.4..8.;

/// An artist's signature, ready for the page to draw.
pub struct Signature {
    /// The SVG document itself.
    pub svg: Arc<[u8]>,
    /// The picture's width over its height, so the page can draw it without stretching it.
    pub aspect: f32,
}

/// Looks the artist's signature up by name, and answers with it or with nothing.
///
/// A service that is down, slow or answers with something that is not a picture is no
/// different from an artist who has never signed anything: the page keeps the typed name.
pub async fn find(http: &reqwest::Client, name: &str) -> Option<Signature> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let mut endpoint = reqwest::Url::parse(ENDPOINT).ok()?;
    endpoint
        .path_segments_mut()
        .ok()?
        .push(name)
        .push("signatures");

    let answer = http
        .get(endpoint)
        .header("User-Agent", AGENT)
        .send()
        .await
        .ok()?;
    if !answer.status().is_success() {
        return None;
    }
    let listing: serde_json::Value = answer.json().await.ok()?;
    let (url, aspect) = pick(&listing)?;

    let picture = http
        .get(url)
        .header("User-Agent", AGENT)
        .send()
        .await
        .ok()?;
    if !picture.status().is_success() {
        return None;
    }
    let svg = picture.bytes().await.ok()?;
    if svg.is_empty() {
        return None;
    }
    Some(Signature {
        svg: Arc::from(&svg[..]),
        aspect,
    })
}

/// The signature to draw and the shape it wants, out of everything the service lists.
///
/// A verified autograph is preferred over one that has not been checked, and past that the
/// first SVG wins. Anything that is not an SVG is left out: the page draws signatures as vector
/// masks in the text colour, which is what keeps them legible on every theme.
fn pick(listing: &serde_json::Value) -> Option<(String, f32)> {
    let signatures = listing.get("signatures")?.as_array()?;
    let mut chosen: Option<&serde_json::Value> = None;
    for signature in signatures {
        let svg = signature.get("format").and_then(|format| format.as_str()) == Some("svg")
            || signature
                .get("url")
                .and_then(|url| url.as_str())
                .is_some_and(|url| {
                    url.split('?')
                        .next()
                        .is_some_and(|url| url.ends_with(".svg"))
                });
        if !svg {
            continue;
        }
        let verified = signature
            .get("verification")
            .and_then(|check| check.as_str())
            == Some("artist_verified");
        match (verified, chosen) {
            (true, _) => {
                chosen = Some(signature);
                break;
            }
            (false, None) => chosen = Some(signature),
            (false, Some(_)) => {}
        }
    }

    let signature = chosen?;
    let url = signature.get("url")?.as_str()?.to_owned();
    let width = signature.get("width").and_then(serde_json::Value::as_f64);
    let height = signature.get("height").and_then(serde_json::Value::as_f64);
    let aspect = match (width, height) {
        (Some(width), Some(height)) if width > 0. && height > 0. => (width / height) as f32,
        _ => GUESSED_ASPECT,
    };
    Some((url, aspect.clamp(ASPECT_RANGE.start, ASPECT_RANGE.end)))
}
