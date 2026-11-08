//! Genius, as a last resort: plain words from a song page.
//!
//! Genius has no public lyric API for a desktop client, so the search endpoint and the page it
//! points at are read directly. The page is HTML and its markup nests the way a browser reads
//! it, so it is parsed rather than scanned: a byte scan of the raw page counts tags inside
//! `<script>` and `<style>` blocks that are not markup, and mis-nests the page around the
//! lyrics.

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use scraper::{ElementRef, Html, Node, Selector};
use serde::Deserialize;

use crate::lyrics::catalog;
use crate::{Lyrics, LyricsHit, LyricsProvider, LyricsQuery};

const SOURCE: &str = "Genius";
const SEARCH: &str = "https://genius.com/api/search/multi";
/// What this says it is, which is deliberately not a browser.
///
/// Genius sits behind Cloudflare, and Cloudflare challenges a browser-claiming User-Agent
/// whose TLS fingerprint and `sec-ch-ua` headers do not back the claim. Measured from this
/// app against both of Genius' endpoints: a Chrome UA is answered `403` with a bot-challenge
/// page, and a plain one is answered `200` with the JSON and the lyrics HTML. Claiming to
/// be a browser was the thing getting this blocked.
///
/// The cost of the 403 was invisible. A failed request travels up as "no lyrics for this
/// track", which is indistinguishable from a song Genius genuinely does not have.
const AGENT: &str = concat!(
    "sonora/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/sonorahq/sonora)"
);

pub struct Genius {
    http: reqwest::Client,
}

impl Genius {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
    }

    async fn search(&self, query: &LyricsQuery) -> Result<Vec<GeniusResult>> {
        let response = self
            .http
            .get(SEARCH)
            .query(&[("q", format!("{} {}", query.title, query.artist))])
            .header("User-Agent", AGENT)
            .header("Accept", "application/json")
            .header("Referer", "https://genius.com/")
            .send()
            .await
            .context("cannot reach Genius")?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("Genius answered with status {status}");
        }
        let answer: Answer = response
            .json()
            .await
            .context("cannot read the Genius response")?;
        Ok(answer
            .response
            .sections
            .into_iter()
            .filter(|section| section.kind == "song")
            .flat_map(|section| section.hits)
            .filter_map(|hit| hit.result)
            .collect())
    }

    async fn page(&self, url: &str) -> Result<String> {
        let response = self
            .http
            .get(url)
            .header("User-Agent", AGENT)
            .header("Accept", "text/html")
            .header("Referer", "https://genius.com/")
            .send()
            .await
            .context("cannot reach the Genius page")?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("the Genius page answered with status {status}");
        }
        response.text().await.context("cannot read the Genius page")
    }
}

impl Default for Genius {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LyricsProvider for Genius {
    fn name(&self) -> &'static str {
        SOURCE
    }

    async fn search(&self, query: &LyricsQuery) -> Result<Vec<LyricsHit>> {
        let results = self.search(query).await?;
        let best = results
            .into_iter()
            .filter_map(|result| {
                let candidate = result.hit();
                crate::lyrics::matched(query, &candidate)
                    .then(|| (crate::lyrics::score(query, &candidate), result))
            })
            .max_by_key(|(score, _)| *score);
        let Some((_, result)) = best else {
            return Ok(Vec::new());
        };

        let html = self.page(&result.url).await?;
        let text = words(&html);
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![catalog::hit_with_trust(
            SOURCE,
            query,
            Lyrics::plain(text),
            5,
        )])
    }
}

#[derive(Deserialize, Default)]
struct Answer {
    #[serde(default)]
    response: Response,
}

#[derive(Deserialize, Default)]
struct Response {
    #[serde(default)]
    sections: Vec<Section>,
}

#[derive(Deserialize, Default)]
struct Section {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    hits: Vec<Hit>,
}

#[derive(Deserialize, Default)]
struct Hit {
    #[serde(default)]
    result: Option<GeniusResult>,
}

#[derive(Deserialize, Default)]
struct GeniusResult {
    #[serde(default)]
    title: String,
    #[serde(default, rename = "artist_names")]
    artist: String,
    #[serde(default)]
    url: String,
}

impl GeniusResult {
    fn hit(&self) -> LyricsHit {
        LyricsHit {
            source: SOURCE,
            trust: 5,
            lyrics: Lyrics::plain(String::new()),
            instrumental: false,
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: None,
            duration: None,
            writers: Vec::new(),
        }
    }
}

/// The classes Genius puts around a song's words that are not its words: the header with its
/// count of contributors and its list of translations, the song's introduction, the adverts,
/// and the buttons and scripts that go with them.
const FURNITURE: [&str; 3] = [
    "LyricsHeader__Container",
    "SongBioPreview__Container",
    "InreadAd__Container",
];

/// The words of a song page, verses separated by a blank line.
fn words(html: &str) -> String {
    let page = Html::parse_document(html);
    let marked = Selector::parse("div[data-lyrics-container=true]")
        .expect("the selector is a constant and always parses");
    let legacy =
        Selector::parse("div.lyrics").expect("the selector is a constant and always parses");
    let mut blocks: Vec<ElementRef<'_>> = page.select(&marked).collect();
    if blocks.is_empty() {
        blocks = page.select(&legacy).collect();
    }

    let mut out = String::new();
    for block in blocks {
        // The words sit inside the same container as the furniture, so the furniture is left
        // out as the words are read rather than sliced away by offset.
        gather(block, &mut out);
        out.push('\n');
    }
    tidy(&out)
}

/// Writes the words under a node, leaving out the furniture and turning `<br>` into a newline.
fn gather<'a>(element: ElementRef<'a>, out: &mut String) {
    for child in element.children() {
        match child.value() {
            Node::Text(text) => out.push_str(&text.text),
            Node::Element(tag) => {
                if furniture(tag) {
                    continue;
                }
                let Some(element) = ElementRef::wrap(child) else {
                    continue;
                };
                match tag.name() {
                    "br" => out.push('\n'),
                    "p" | "div" => {
                        out.push('\n');
                        gather(element, out);
                        out.push('\n');
                    }
                    _ => gather(element, out),
                }
            }
            _ => {}
        }
    }
}

/// Whether an element holds a song page's furniture rather than its words.
fn furniture(tag: &scraper::node::Element) -> bool {
    if matches!(tag.name(), "script" | "style" | "button") {
        return true;
    }
    if tag.attr("data-exclude-from-selection") == Some("true") {
        return true;
    }
    tag.attr("class")
        .is_some_and(|classes| FURNITURE.iter().any(|name| classes.contains(name)))
}

/// Cleans the text a page carries around its words: the spaces Genius writes in special
/// unicode, its "You might also like" insertions, and the "29 Embed" footer.
fn tidy(raw: &str) -> String {
    let raw = raw
        .replace('\u{00A0}', " ")
        .replace('\u{200B}', " ")
        .replace('\u{FEFF}', " ");
    let mut out = String::with_capacity(raw.len());
    for line in raw.lines() {
        let line = line.trim().replace("You might also like", "");
        let line = trim_embed(line.trim());
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    let mut lines: Vec<&str> = out.lines().collect();
    while lines.first().is_some_and(|line| line.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let mut text = lines.join("\n");
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }
    text
}

/// A line without its trailing "Embed" count.
fn trim_embed(line: &str) -> &str {
    let trimmed = line.trim_end();
    let Some(head) = trimmed.strip_suffix("Embed") else {
        return trimmed;
    };
    head.trim_end()
        .trim_end_matches(|character: char| character.is_ascii_digit())
        .trim_end()
}

#[cfg(test)]
mod tests {
    use super::words;

    #[test]
    fn reads_the_marked_container() {
        let html = r#"<html><body><div data-lyrics-container="true" class="x">First line<br>Second &amp; last<br><br>Another verse</div></body></html>"#;
        assert_eq!(words(html), "First line\nSecond & last\n\nAnother verse");
    }

    #[test]
    fn skips_nested_divs() {
        let html =
            r#"<div data-lyrics-container="true">A<div>inside</div>B</div><div>outside</div>"#;
        assert_eq!(words(html), "A\ninside\nB");
    }

    #[test]
    fn leaves_out_the_furniture() {
        let html = r#"<div data-lyrics-container="true">
            <div class="LyricsHeader__Container-sc">98 Contributors Translations</div>
            [Verse 1]<br>I met him at a party<br><br>[Chorus]<br>These days, I've got expectations
            <div class="LyricsFooter__Container-sc"><button>Share</button>29 Embed</div>
        </div>"#;
        assert_eq!(
            words(html),
            "[Verse 1]\nI met him at a party\n\n[Chorus]\nThese days, I've got expectations"
        );
    }

    #[test]
    fn ignores_tag_shaped_text_in_a_script() {
        let html = r#"<script>var markup = "<div><div>";</script>
            <div data-lyrics-container="true">One<br>Two</div>"#;
        assert_eq!(words(html), "One\nTwo");
    }
}
