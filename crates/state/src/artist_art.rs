//! The art an artist's page puts where the typed name would be: the artist's own autograph,
//! or the title logo Apple Music sets for them, and the photograph behind the header.
//!
//! Autographs are kept by a small service that checks each one against the artist it is
//! credited to. That service is self-hosted and may be asleep when the page asks, so a lookup
//! is retried once and every settled answer — including "this artist has none" — is kept for
//! the rest of the session.
//!
//! Apple Music's artist pages carry a transparent title logo for some artists, and the same
//! two public, unauthenticated requests BitChord uses find it: the iTunes search turns a name
//! into a candidate, the artist's own page carries the logo in the JSON it embeds for
//! hydration. A name is not an identity, so a candidate is only taken when its catalogue
//! shares a song or release title with the page's own.

use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use serde_json::Value;

/// Where an artist's signatures are looked up.
const ENDPOINT: &str = "https://artistsignature.velixir.run/v1/signatures";
/// The agent every request here is made with, so a request that goes wrong is recognisable.
const AGENT: &str = concat!(
    "sonora/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/sonorahq/sonora)"
);
/// Apple answers a browser and is unhelpful without one.
const BROWSER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/124.0 Safari/537.36";
/// How long one request may take before it is treated as a miss.
const DEADLINE: Duration = Duration::from_secs(12);
/// How long a failed lookup waits before it is tried once more, which is enough for a
/// self-hosted service that has just been woken to answer.
const RETRY: Duration = Duration::from_secs(3);
/// The width the title logo is asked for. Apple's URL template scales on request.
const LOGO_WIDTH: u32 = 1000;
/// How wide a signature is guessed to be when the service does not say.
const GUESSED_ASPECT: f32 = 2.5;
/// The shapes a mark is allowed to be drawn in. Past these it is either a very short monogram
/// or a very long flourish, and either one would be drawn wrong by a guess.
const ASPECT_RANGE: Range<f32> = 0.4..8.;
/// How much of a raster signature must be near-white paper for it to read as a scan.
const SCAN_WHITE: f32 = 0.55;
/// A pixel above this luminance is paper rather than ink.
const PAPER_LUMINANCE: f32 = 0.92;

/// The picture a signature is drawn from.
#[derive(Clone)]
pub enum Ink {
    /// An SVG document, drawn as a mask in the page's text colour and so legible on any theme.
    Vector(Arc<[u8]>),
    /// A picture with its own pixels: white ink on transparency for a scanned autograph, or
    /// the transparent PNG Apple Music sets a title in.
    Raster(Arc<[u8]>),
}

/// An artist's mark, ready for the page to draw.
#[derive(Clone)]
pub struct Signature {
    pub ink: Ink,
    /// The picture's width over its height, so the page can draw it without stretching it.
    pub aspect: f32,
}

/// Answers already settled this session, keyed by the folded artist name and its titles. Both
/// hits and genuine misses are kept: revisiting an artist must not wake the service again.
static KNOWN: LazyLock<Mutex<HashMap<String, Option<Signature>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Looks the artist's mark up, and answers with it or with nothing.
///
/// Apple Music's title logo is asked for first, as BitChord's phone does, and the community
/// signature service is asked at the same time for the artists Apple has nothing for — the two
/// requests run together so the page waits on the slower one rather than on both. A service
/// that is down, slow or answers with something that is not a picture is no different from an
/// artist who has never signed anything: the page keeps the typed name.
///
/// A mark that answers is written to the disk cache, so the next session draws it without a
/// request at all.
pub async fn find(http: &reqwest::Client, name: &str, titles: &[String]) -> Option<Signature> {
    let name = name.trim();
    let key = fold(name);
    if key.is_empty() {
        return None;
    }
    let keys = titles
        .iter()
        .filter_map(|title| title_key(title))
        .collect::<BTreeSet<_>>();
    let cache = cache_key(&key, &keys);
    if let Some(known) = KNOWN
        .lock()
        .ok()
        .and_then(|known| known.get(&cache).cloned())
    {
        return known;
    }
    if let Some(kept) = kept(&cache) {
        log::info!("artist: {name}'s mark comes from the disk cache");
        remember(&cache, Some(kept.clone()));
        return Some(kept);
    }

    let (apple, signed) = futures::join!(apple(http, name, &key, &keys), signed(http, name));
    let mut settled = true;
    let (mark, source) = match apple {
        Attempt::Found(signature) => (Some(signature), "apple's title logo"),
        Attempt::Missing => match signed {
            Attempt::Found(signature) => (Some(signature), "the community service"),
            Attempt::Missing => (None, ""),
            Attempt::Failed => {
                log::warn!("artist: the signature service could not be reached for {name}");
                settled = false;
                (None, "")
            }
        },
        Attempt::Failed => {
            log::warn!("artist: apple's page for {name} could not be reached");
            settled = false;
            match signed {
                Attempt::Found(signature) => (Some(signature), "the community service"),
                _ => (None, ""),
            }
        }
    };

    let Some(mark) = mark else {
        if settled {
            remember(&cache, None);
        }
        return None;
    };
    log::info!("artist: {name}'s mark comes from {source}");
    keep(&cache, &mark);
    remember(&cache, Some(mark.clone()));
    Some(mark)
}

/// The cache key for an artist: the folded name, and the titles the page vouches for, since
/// two artists can share a name and each is answered for its own songs.
fn cache_key(key: &str, keys: &BTreeSet<String>) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    for title in keys {
        title.hash(&mut hasher);
    }
    format!("{key}:{:016x}", hasher.finish())
}

/// Keeps a settled answer, so a revisit costs nothing.
fn remember(cache: &str, mark: Option<Signature>) {
    if let Ok(mut known) = KNOWN.lock() {
        known.insert(cache.to_owned(), mark);
    }
}

/// The marks kept between sessions: one file an artist, beside the cache database. Apple's page
/// and the signature service are both slow enough that asking again on every launch is the
/// difference between a name and a mark.
fn folder() -> Option<std::path::PathBuf> {
    let dir = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("sonora")
        .join("artist-marks");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// The file one artist's mark lives in. The key is folded to letters, digits and dashes so it is
/// a name every filesystem takes.
fn path(cache: &str) -> Option<std::path::PathBuf> {
    let name = cache.replace(':', "-");
    Some(folder()?.join(format!("{name}.mark")))
}

/// How old a kept mark may be before it is asked for again. Both services gain and correct
/// entries over time, and a month-old answer is worth one request to keep up with them.
const STALE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// A mark kept from an earlier session. One small header carries what the ink is and what shape
/// it has, and the rest of the file is the picture itself.
fn kept(cache: &str) -> Option<Signature> {
    let path = path(cache)?;
    let age = std::fs::metadata(&path)
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .unwrap_or_default();
    if age > STALE {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let (head, ink) = bytes.split_at_checked(5)?;
    let aspect = f32::from_le_bytes(head[1..5].try_into().ok()?);
    if !aspect.is_finite() || aspect <= 0. {
        return None;
    }
    let ink = Arc::from(ink);
    Some(Signature {
        ink: match head[0] {
            0 => Ink::Vector(ink),
            _ => Ink::Raster(ink),
        },
        aspect,
    })
}

/// Writes a mark down for the next session, and keeps the folder from growing without bound.
fn keep(cache: &str, mark: &Signature) {
    let Some(path) = path(cache) else {
        return;
    };
    let (kind, ink) = match &mark.ink {
        Ink::Vector(ink) => (0u8, ink),
        Ink::Raster(ink) => (1u8, ink),
    };
    let mut bytes = Vec::with_capacity(ink.len() + 5);
    bytes.push(kind);
    bytes.extend_from_slice(&mark.aspect.to_le_bytes());
    bytes.extend_from_slice(ink);
    if std::fs::write(&path, bytes).is_err() {
        return;
    }
    prune(&path);
}

/// How many marks the folder keeps. Every one is a picture a page can ask for again, so the
/// oldest go once there are more than this, and a session that visits many artists does not
/// grow the cache without end.
const KEPT: usize = 200;

fn prune(newest: &std::path::Path) {
    let Some(folder) = folder() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&folder) else {
        return;
    };
    let mut marks = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|kind| kind == "mark"))
        .filter_map(|entry| {
            let touched = entry.metadata().ok()?.modified().ok()?;
            Some((touched, entry.path()))
        })
        .collect::<Vec<_>>();
    if marks.len() <= KEPT {
        return;
    }
    marks.sort();
    let drop = marks.len() - KEPT;
    for (_, path) in marks.into_iter().take(drop) {
        if path != newest {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// What one lookup settled on. A failure is not an answer: it is not remembered, and the next
/// visit to the page asks again.
enum Attempt<T> {
    Found(T),
    Missing,
    Failed,
}

/// The artist's own autograph, from the signature service.
///
/// The service is self-hosted and may be asleep when the page opens, so a failed request is
/// tried twice more with growing pauses. A settled answer — including "no signature" — is
/// returned at once; only a failure waits.
async fn signed(http: &reqwest::Client, name: &str) -> Attempt<Signature> {
    for pause in [Duration::ZERO, RETRY, RETRY * 2 + Duration::from_secs(2)] {
        if !pause.is_zero() {
            tokio::time::sleep(pause).await;
        }
        match signed_once(http, name).await {
            Attempt::Failed => {}
            settled => return settled,
        }
    }
    Attempt::Failed
}

async fn signed_once(http: &reqwest::Client, name: &str) -> Attempt<Signature> {
    let Ok(mut endpoint) = reqwest::Url::parse(ENDPOINT) else {
        return Attempt::Failed;
    };
    endpoint.query_pairs_mut().append_pair("artist", name);
    let answer = http
        .get(endpoint)
        .header("User-Agent", AGENT)
        .timeout(DEADLINE)
        .send()
        .await;
    let answer = match answer {
        Ok(answer) => answer,
        Err(_) => return Attempt::Failed,
    };
    if answer.status() == reqwest::StatusCode::NOT_FOUND {
        return Attempt::Missing;
    }
    if !answer.status().is_success() {
        return Attempt::Failed;
    }
    let Ok(listing) = answer.json::<Value>().await else {
        return Attempt::Failed;
    };
    // An ambiguous name is answered with candidates rather than a guess, and the page must
    // not put someone else's autograph under this artist's name.
    if listing.get("signatures").is_none() {
        return Attempt::Missing;
    }

    for candidate in ranked(&listing) {
        match picture(http, &candidate).await {
            Ok(Some(ink)) => {
                return Attempt::Found(Signature {
                    ink,
                    aspect: candidate.aspect,
                });
            }
            Ok(None) => {}
            Err(()) => return Attempt::Failed,
        }
    }
    Attempt::Missing
}

/// One autograph the service lists, with the rank that decides which is drawn.
struct Candidate {
    url: String,
    format: String,
    aspect: f32,
    score: i32,
    area: f64,
}

/// The listed signatures best first: the service's own order of preference, as BitChord reads
/// it — a known licence first, then a vector, then how it was verified, then a wide shape,
/// which is usually a complete line rather than a fragment.
fn ranked(listing: &Value) -> Vec<Candidate> {
    let Some(signatures) = listing.get("signatures").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut candidates = signatures
        .iter()
        .filter_map(candidate)
        .collect::<Vec<Candidate>>();
    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then(right.area.total_cmp(&left.area))
    });
    candidates
}

fn candidate(signature: &Value) -> Option<Candidate> {
    let asset = signature.get("asset").and_then(Value::as_object);
    let url = asset
        .and_then(|asset| asset.get("url"))
        .and_then(Value::as_str)
        .or_else(|| signature.get("url").and_then(Value::as_str))
        .or_else(|| signature.get("original_url").and_then(Value::as_str))
        .or_else(|| signature.get("originalUrl").and_then(Value::as_str))?
        .to_owned();
    if !url.starts_with("http") {
        return None;
    }
    let format = asset
        .and_then(|asset| asset.get("format"))
        .and_then(Value::as_str)
        .or_else(|| signature.get("format").and_then(Value::as_str))
        .map(|format| format.to_lowercase())
        .unwrap_or_else(|| {
            url.split('?')
                .next()
                .and_then(|url| url.rsplit('.').next())
                .unwrap_or_default()
                .to_lowercase()
        });
    let width = asset
        .and_then(|asset| asset.get("width"))
        .and_then(Value::as_f64)
        .or_else(|| signature.get("width").and_then(Value::as_f64));
    let height = asset
        .and_then(|asset| asset.get("height"))
        .and_then(Value::as_f64)
        .or_else(|| signature.get("height").and_then(Value::as_f64));
    let aspect = match (width, height) {
        (Some(width), Some(height)) if width > 0. && height > 0. => (width / height) as f32,
        _ => GUESSED_ASPECT,
    };
    let score = score(signature, &format, aspect);
    let area = width.unwrap_or(0.).max(0.) * height.unwrap_or(0.).max(0.);
    Some(Candidate {
        url,
        format,
        aspect: aspect.clamp(ASPECT_RANGE.start, ASPECT_RANGE.end),
        score,
        area,
    })
}

/// The same order BitChord's phone ranks signatures in, so the two draw the same autograph.
fn score(signature: &Value, format: &str, aspect: f32) -> i32 {
    let licence = signature
        .get("license")
        .and_then(|licence| licence.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let licence = match licence {
        "known" => 2,
        "requires_review" => 1,
        _ => 0,
    };
    let format = match format {
        "svg" => 2,
        "png" => 1,
        _ => 0,
    };
    let verification = signature
        .get("verification")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let verification = match verification {
        "artist_verified" => 3,
        "verified" => 2,
        "unverified" => 1,
        _ => 0,
    };
    let panoramic = i32::from(aspect >= 1.4);
    licence * 1000 + format * 100 + verification * 10 + panoramic
}

/// Fetches one signature and turns it into something the page can draw. `Ok(None)` means the
/// picture is not an autograph after all, and the caller should try the next candidate.
async fn picture(http: &reqwest::Client, candidate: &Candidate) -> Result<Option<Ink>, ()> {
    let answer = http
        .get(&candidate.url)
        .header("User-Agent", AGENT)
        .timeout(DEADLINE)
        .send()
        .await
        .map_err(|_| ())?;
    if !answer.status().is_success() {
        return Ok(None);
    }
    let bytes = answer.bytes().await.map_err(|_| ())?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let vector = candidate.format == "svg" || looks_like_svg(&bytes);
    match vector {
        true => Ok(Some(Ink::Vector(Arc::from(&bytes[..])))),
        false => Ok(ink(&bytes).map(|picture| Ink::Raster(Arc::from(picture)))),
    }
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let start = &bytes[..bytes.len().min(512)];
    String::from_utf8_lossy(start).contains("<svg")
}

/// Turns a scanned autograph into white ink on transparency, so it reads over the artist's
/// photograph whatever the paper looked like.
///
/// Dark strokes on transparency keep their own alpha and become white. A scan on white paper
/// gets each pixel's darkness as its alpha, which drops the paper away. A photograph — no
/// paper and no transparency — is refused: it is not a signature and must not become a white
/// blob.
fn ink(bytes: &[u8]) -> Option<Vec<u8>> {
    let picture = image::load_from_memory(bytes).ok()?.into_rgba8();
    let total = (picture.width() as usize) * (picture.height() as usize);
    if total == 0 {
        return None;
    }
    let mut paper = 0usize;
    let mut clear = 0usize;
    for pixel in picture.pixels() {
        if pixel[3] == 0 {
            clear += 1;
        } else if luminance(pixel) >= PAPER_LUMINANCE {
            paper += 1;
        }
    }
    let scanned = paper as f32 / total as f32 >= SCAN_WHITE;
    if !scanned && clear == 0 {
        return None;
    }

    let mut inked = picture;
    for pixel in inked.pixels_mut() {
        let alpha = pixel[3] as f32 / 255.;
        if alpha <= 0. {
            *pixel = image::Rgba([0, 0, 0, 0]);
            continue;
        }
        let darkness = match scanned {
            true => 1. - luminance(pixel),
            false => alpha,
        };
        let value = (darkness.clamp(0., 1.) * 255.).round() as u8;
        *pixel = image::Rgba([255, 255, 255, value]);
    }

    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(inked)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

fn luminance(pixel: &image::Rgba<u8>) -> f32 {
    (0.299 * pixel[0] as f32 + 0.587 * pixel[1] as f32 + 0.114 * pixel[2] as f32) / 255.
}

/// Apple Music's title logo for the artist, when Apple has one and the candidate's catalogue
/// proves it is the same artist.
async fn apple(
    http: &reqwest::Client,
    name: &str,
    key: &str,
    keys: &BTreeSet<String>,
) -> Attempt<Signature> {
    if keys.is_empty() {
        return Attempt::Missing;
    }
    let Ok(mut search) = reqwest::Url::parse("https://itunes.apple.com/search") else {
        return Attempt::Failed;
    };
    search
        .query_pairs_mut()
        .append_pair("term", name)
        .append_pair("entity", "musicArtist")
        .append_pair("limit", "5");
    let listing = match json(http, search.as_str(), AGENT).await {
        Ok(listing) => listing,
        Err(()) => return Attempt::Failed,
    };
    let Some(results) = listing.get("results").and_then(Value::as_array) else {
        return Attempt::Missing;
    };

    for result in results {
        if result
            .get("artistName")
            .and_then(Value::as_str)
            .map(fold)
            .as_deref()
            != Some(key)
        {
            continue;
        }
        let Some(id) = result.get("artistId").and_then(Value::as_i64) else {
            continue;
        };
        match shares_titles(http, id, keys).await {
            Ok(true) => {}
            Ok(false) => continue,
            Err(()) => return Attempt::Failed,
        }
        let page = result
            .get("artistLinkUrl")
            .and_then(Value::as_str)
            .map(|url| url.split('?').next().unwrap_or(url).to_owned())
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| format!("https://music.apple.com/us/artist/{id}"));
        match apple_page(http, &page).await {
            Ok(Some(logo)) => return Attempt::Found(logo),
            Ok(None) => return Attempt::Missing,
            Err(()) => return Attempt::Failed,
        }
    }
    Attempt::Missing
}

/// Whether Apple artist `id` has a song or release whose folded title is one the page vouches
/// for. One request covers both: each track row carries its album's name.
async fn shares_titles(
    http: &reqwest::Client,
    id: i64,
    keys: &BTreeSet<String>,
) -> Result<bool, ()> {
    let mut lookup = reqwest::Url::parse("https://itunes.apple.com/lookup").map_err(|_| ())?;
    lookup
        .query_pairs_mut()
        .append_pair("id", &id.to_string())
        .append_pair("entity", "song")
        .append_pair("limit", "200");
    let listing = json(http, lookup.as_str(), AGENT).await?;
    let rows = listing.get("results").and_then(Value::as_array);
    Ok(rows.is_some_and(|rows| {
        rows.iter().any(|row| {
            ["trackName", "collectionName"].iter().any(|field| {
                row.get(*field)
                    .and_then(Value::as_str)
                    .and_then(title_key)
                    .is_some_and(|title| keys.contains(&title))
            })
        })
    }))
}

/// Reads the title logo out of the artist page's embedded JSON.
async fn apple_page(http: &reqwest::Client, page: &str) -> Result<Option<Signature>, ()> {
    let answer = http
        .get(page)
        .header("User-Agent", BROWSER_AGENT)
        .timeout(DEADLINE)
        .send()
        .await
        .map_err(|_| ())?;
    if !answer.status().is_success() {
        return Ok(None);
    }
    let html = answer.text().await.map_err(|_| ())?;
    let Some(json) = embedded(&html) else {
        return Ok(None);
    };
    let Ok(page) = serde_json::from_str::<Value>(&json) else {
        return Ok(None);
    };
    let Some(header) = find_header(&page) else {
        return Ok(None);
    };

    let Some(dictionary) = header
        .get("artistLogo")
        .and_then(|logo| logo.get("dictionary"))
    else {
        return Ok(None);
    };
    let Some(url) = fill(dictionary, LOGO_WIDTH, "png") else {
        return Ok(None);
    };
    match picture_bytes(http, &url).await? {
        Some(bytes) => Ok(Some(Signature {
            ink: Ink::Raster(Arc::from(&bytes[..])),
            aspect: aspect(dictionary),
        })),
        None => Ok(None),
    }
}

/// A picture's width over its height, as Apple states it.
fn aspect(dictionary: &Value) -> f32 {
    let width = dictionary
        .get("width")
        .and_then(Value::as_f64)
        .unwrap_or(0.);
    let height = dictionary
        .get("height")
        .and_then(Value::as_f64)
        .unwrap_or(0.);
    if width <= 0. || height <= 0. {
        return 1.;
    }
    (width / height) as f32
}

/// The picture Apple serves, at the width the page draws it. The URL carries the size and
/// the format as placeholders, which the service fills in on request.
fn fill(dictionary: &Value, width: u32, format: &str) -> Option<String> {
    let template = dictionary.get("url").and_then(Value::as_str)?;
    if !template.contains("{w}") {
        return None;
    }
    let wide = dictionary.get("width").and_then(Value::as_f64)?;
    let tall = dictionary.get("height").and_then(Value::as_f64)?;
    if wide <= 0. || tall <= 0. {
        return None;
    }
    let height = (width as f64 * tall / wide).round() as u32;
    Some(
        template
            .replace("{w}", &width.to_string())
            .replace("{h}", &height.to_string())
            .replace("{c}", "bb")
            .replace("{f}", format),
    )
}

/// The bytes of a picture Apple serves, when it serves one.
async fn picture_bytes(http: &reqwest::Client, url: &str) -> Result<Option<Vec<u8>>, ()> {
    let picture = http
        .get(url)
        .header("User-Agent", BROWSER_AGENT)
        .timeout(DEADLINE)
        .send()
        .await
        .map_err(|_| ())?;
    if !picture.status().is_success() {
        return Ok(None);
    }
    let bytes = picture.bytes().await.map_err(|_| ())?;
    Ok((!bytes.is_empty()).then(|| bytes.to_vec()))
}

/// The JSON inside the `<script id="serialized-server-data">` tag Apple hydrates its pages
/// with. The script body is a bare value, not an object, so it is wrapped before parsing.
fn embedded(html: &str) -> Option<String> {
    let marker = html.find("id=\"serialized-server-data\"")?;
    let start = html[marker..].find('>')? + marker + 1;
    let end = html[start..].find("</script>")? + start;
    Some(format!("{{\"d\":{}}}", &html[start..end]))
}

/// The first object anywhere in the tree that carries an `artistLogo`, which is the page's
/// header rather than one of the shelves under it.
fn find_header(node: &Value) -> Option<&Value> {
    match node {
        Value::Object(map) => {
            if map.contains_key("artistLogo") {
                return Some(node);
            }
            map.values().find_map(find_header)
        }
        Value::Array(items) => items.iter().find_map(find_header),
        _ => None,
    }
}

/// Fetches and parses one JSON document, treating any failure as a failed attempt.
async fn json(http: &reqwest::Client, url: &str, agent: &str) -> Result<Value, ()> {
    let answer = http
        .get(url)
        .header("User-Agent", agent)
        .timeout(DEADLINE)
        .send()
        .await
        .map_err(|_| ())?;
    if !answer.status().is_success() {
        return Err(());
    }
    answer.json().await.map_err(|_| ())
}

/// Case, accents and punctuation are not what makes two names different, and a `$` standing in
/// for an S is read as one: Apple's KR$NA is YouTube's Krsna.
fn fold(value: &str) -> String {
    deunicode::deunicode(&value.to_lowercase().replace('$', "s"))
        .chars()
        .filter(|letter| letter.is_ascii_alphanumeric())
        .collect()
}

/// A title reduced to what both services agree on: Apple appends " - Single" or " - EP" and
/// folds features into the title, YouTube does neither. Too short a title ("War", "Intro")
/// proves nothing and is dropped.
fn title_key(title: &str) -> Option<String> {
    let base = title.split(" (").next().unwrap_or(title);
    let base = base.split(" [").next().unwrap_or(base);
    let base = base.strip_suffix(" - Single").unwrap_or(base);
    let base = base.strip_suffix(" - EP").unwrap_or(base);
    let key = fold(base);
    (key.chars().count() >= 4).then_some(key)
}
