//! Translating lyrics on demand, the way the player draws a translation under the line.
//!
//! Nothing is installed for this: a model for every language would cost tens of megabytes
//! each, and translated lyrics are a few kilobytes. The lines go out in one request,
//! separated by a marker the service passes through untouched, and come back in the same
//! order.

use anyhow::{Context as _, Result};

/// The endpoint that answers with a translation.
const ENDPOINT: &str = "https://translate.googleapis.com/translate_a/single";
/// The agent the endpoint is asked with. It is the same agent the other services here are
/// asked with, so a request that goes wrong is recognisable in a log.
const AGENT: &str = concat!(
    "sonora/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/sonorahq/sonora)"
);

/// How much text goes out in one request. A verse is a few hundred characters, so this is a
/// whole song's worth of lines in one go.
const MAX_BATCH_CHARS: usize = 3_500;

/// The private-use mark that tells one line from the next inside a request. The service
/// passes it through untouched, which is what makes a batch come back in one piece.
const MARKER: char = '\u{E000}';
/// The other end of the mark, which the service passes through as well.
const MARKER_END: char = '\u{E001}';

/// One language lyrics can be translated into.
pub struct TranslationLanguage {
    /// The code the endpoint is given, spelled as the service spells it.
    pub code: &'static str,
    /// The language's name in English, which is how it is listed.
    pub name: &'static str,
}

/// The language lyrics are translated into.
///
/// Only English for now, since that is the one this player is read in. The service takes
/// any code, so widening this list is a matter of adding entries; the endpoint and the
/// request shape do not change.
pub const LANGUAGES: &[TranslationLanguage] = &[TranslationLanguage {
    code: "en",
    name: "English",
}];

/// The language with the given code, if it is one of the languages offered.
pub fn language(code: &str) -> Option<&'static TranslationLanguage> {
    LANGUAGES.iter().find(|language| language.code == code)
}

/// Translates `texts` into `target`, one answer for each.
///
/// The lines are asked for in as few requests as they fit into, and the answers come back in
/// the order they went out. A line the service leaves blank fails the whole call rather than
/// coming back empty, because a translation with a hole in it reads as a mistake.
pub async fn translate(
    http: &reqwest::Client,
    texts: &[String],
    target: &str,
) -> Result<Vec<String>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let mut answers = Vec::with_capacity(texts.len());
    for batch in batches(texts) {
        answers.extend(ask(http, &batch, target).await?);
    }
    Ok(answers)
}

/// The lines that go out in one request, split so that no request carries too much.
fn batches(texts: &[String]) -> Vec<Vec<String>> {
    let mut batches = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut length = 0;
    for text in texts {
        let added = text.len() + if current.is_empty() { 0 } else { 8 };
        if !current.is_empty() && length + added > MAX_BATCH_CHARS {
            batches.push(std::mem::take(&mut current));
            length = 0;
        }
        length += added;
        current.push(text.clone());
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

/// Asks the service for one batch of lines.
async fn ask(http: &reqwest::Client, texts: &[String], target: &str) -> Result<Vec<String>> {
    let payload = payload(texts);
    let response = http
        .post(ENDPOINT)
        .form(&[
            ("client", "gtx"),
            ("sl", "auto"),
            ("tl", target),
            ("dt", "t"),
            ("q", &payload),
        ])
        .header("User-Agent", AGENT)
        .send()
        .await
        .context("cannot reach the translation service")?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("the translation service answered with status {status}");
    }
    let root: serde_json::Value = response
        .json()
        .await
        .context("cannot read the translation service's answer")?;
    let segments = root
        .get(0)
        .and_then(|segments| segments.as_array())
        .context("the translation service's answer holds no text")?;
    let mut body = String::new();
    for segment in segments {
        if let Some(text) = segment.get(0).and_then(|text| text.as_str()) {
            body.push_str(text);
        }
    }
    split(&body, texts.len())
        .context("the translation service's answer does not match what was sent")
}

/// The lines of one request, separated by the marker.
fn payload(texts: &[String]) -> String {
    let mut payload = String::new();
    for (index, text) in texts.iter().enumerate() {
        if index > 0 {
            payload.push('\n');
            payload.push(MARKER);
            payload.push_str(&format!("{index:04}"));
            payload.push(MARKER_END);
            payload.push('\n');
        }
        payload.push_str(text);
    }
    payload
}

/// The lines of an answer, taken apart at the markers.
fn split(body: &str, count: usize) -> Option<Vec<String>> {
    let mut parts = Vec::with_capacity(count);
    let mut rest = body;
    for index in 0..count {
        let part = match index + 1 < count {
            true => {
                let at = rest.find(MARKER)?;
                let (part, tail) = rest.split_at(at);
                let end = tail.find(MARKER_END)? + MARKER_END.len_utf8();
                rest = &tail[end..];
                part
            }
            false => rest,
        };
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        parts.push(part.to_owned());
    }
    Some(parts)
}
