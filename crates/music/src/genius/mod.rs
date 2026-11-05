//! Genius, as a last resort: plain words from a song page.
//!
//! Genius has no public lyric API for a desktop client, so the search endpoint and the page it
//! points at are read directly. The page is HTML, but the lyrics sit in a handful of marked
//! containers with `<br>` between lines, which is little enough to read without a DOM.

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use serde::Deserialize;

use crate::lyrics::{catalog, entities};
use crate::{Lyrics, LyricsHit, LyricsProvider, LyricsQuery};

const SOURCE: &str = "Genius";
const SEARCH: &str = "https://genius.com/api/search/multi";
const AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

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

/// The words of a song page, verses separated by a blank line.
fn words(html: &str) -> String {
    let mut out = String::new();
    for block in blocks(html) {
        out.push_str(&clean(block));
        out.push('\n');
    }
    let out = out
        .replace("You might also like", "")
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n");
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

/// The inside of every `data-lyrics-container="true"` div.
fn blocks(html: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = html[from..].find("data-lyrics-container=\"true\"") {
        let at = from + offset;
        let Some(open) = html[..at].rfind('<') else {
            break;
        };
        let Some(close) = inner_end(html, open) else {
            break;
        };
        found.push(&html[open..close]);
        from = close;
    }
    found
}

/// Where the div that starts at `open` closes, after its opening tag.
fn inner_end(html: &str, open: usize) -> Option<usize> {
    let tag_end = html[open..].find('>')? + open;
    if html[open..tag_end].trim_end().ends_with('/') {
        return Some(tag_end + 1);
    }

    let mut depth = 1usize;
    let mut cursor = tag_end + 1;
    while let Some(offset) = html[cursor..].find('<') {
        let at = cursor + offset;
        let close = html[at..].find('>').map(|end| at + end)?;
        let inner = html[at + 1..close].trim();
        if inner.starts_with("!--") {
            cursor = close + 1;
            continue;
        }
        let closing = inner.starts_with('/');
        let name: String = inner
            .trim_start_matches('/')
            .chars()
            .take_while(|character| character.is_ascii_alphanumeric())
            .collect();
        if name.eq_ignore_ascii_case("div") {
            if closing {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(at);
                }
            } else if !inner.ends_with('/') {
                depth += 1;
            }
        }
        cursor = close + 1;
    }
    None
}

/// Tags out, line breaks in, entities decoded.
fn clean(block: &str) -> String {
    let mut out = String::with_capacity(block.len());
    let mut rest = block;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>').map(|end| open + end) else {
            break;
        };
        let tag = &rest[open + 1..close];
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|character| character.is_ascii_alphanumeric())
            .collect();
        if name.eq_ignore_ascii_case("br")
            || name.eq_ignore_ascii_case("p")
            || name.eq_ignore_ascii_case("div")
        {
            out.push('\n');
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    entities::decode(&out)
}

#[cfg(test)]
mod tests {
    use super::{clean, words};

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
    fn strips_tags_but_keeps_breaks() {
        assert_eq!(clean("<b>One</b><br/>Two"), "One\nTwo");
    }
}
