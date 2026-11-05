//! QQ Music's karaoke lyrics, as BetterLyrics and the QQ catalogue send them.
//!
//! The document is an XML wrapper around one escaped `LyricContent` attribute. Each line is
//! `[line-start,line-duration]` followed by words timed `(start,duration)`. Neither timing is a
//! clock stamp: both count milliseconds from the beginning of the song.

use std::time::Duration;

use crate::lyrics::{entities, lrc};
use crate::{LyricsLine, LyricsWord, Voice};

const ATTRIBUTE: &str = "LyricContent";

/// Parses a QRC document or a bare block of QRC lines.
pub(crate) fn parse(document: &str) -> Vec<LyricsLine> {
    let content = attribute(document).unwrap_or_else(|| document.to_owned());
    let mut lines: Vec<LyricsLine> = content.lines().filter_map(line).collect();
    lrc::normalize(&mut lines);
    lines
}

/// The escaped attribute value, when the document is the XML wrapper rather than lines.
fn attribute(document: &str) -> Option<String> {
    let lower = document.to_ascii_lowercase();
    let at = lower.find(&ATTRIBUTE.to_ascii_lowercase())?;
    let after = &document[at + ATTRIBUTE.len()..];
    let equals = after.find('=')?;
    let after = after[equals + 1..].trim_start();
    let quote = after
        .chars()
        .next()
        .filter(|quote| *quote == '"' || *quote == '\'')?;
    let body = &after[1..];
    let end = body.find(quote)?;
    Some(entities::decode(&body[..end]))
}

fn line(text: &str) -> Option<LyricsLine> {
    let text = text.trim();
    let rest = text.strip_prefix('[')?;
    let (times, body) = rest.split_once(']')?;
    let (start, length) = times.split_once(',')?;
    let start = start.trim().parse::<u64>().ok()?;
    let length = length.trim().parse::<u64>().ok()?;

    let groups = groups(body);
    let text = entities::decode(&without_times(body)).trim().to_owned();
    if text.is_empty() {
        return None;
    }

    let words = match words(body, &groups) {
        words if words.is_empty() => None,
        words => Some(words),
    };
    Some(LyricsLine {
        start: Duration::from_millis(start),
        end: (length > 0).then(|| Duration::from_millis(start.saturating_add(length))),
        text,
        romanized: None,
        words,
        secondary: Vec::new(),
        voice: Voice::default(),
    })
}

struct Timed {
    open: usize,
    close: usize,
    start: u64,
    length: u64,
}

fn groups(body: &str) -> Vec<Timed> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(open) = body[from..].find('(').map(|at| from + at) {
        let Some(close) = body[open..].find(')').map(|at| open + at) else {
            break;
        };
        let inner = &body[open + 1..close];
        let numbers: Vec<u64> = inner
            .split(',')
            .filter_map(|number| number.trim().parse().ok())
            .collect();
        if numbers.len() >= 2 {
            found.push(Timed {
                open,
                close,
                start: numbers[0],
                length: numbers[1],
            });
        }
        from = close + 1;
    }
    found
}

/// Text follows each timing group. `(0,500)Hello (500,500)world`
fn prefix(body: &str, groups: &[Timed]) -> Vec<LyricsWord> {
    let mut words = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        let from = group.close + 1;
        let to = groups
            .get(index + 1)
            .map(|next| next.open)
            .unwrap_or(body.len());
        let text = entities::decode(body[from..to].trim());
        if text.is_empty() {
            continue;
        }
        words.push(LyricsWord {
            start: Duration::from_millis(group.start),
            end: Duration::from_millis(group.start.saturating_add(group.length)),
            text,
        });
    }
    words
}

/// Text precedes each timing group. `Hello(0,500) world(500,500)`
fn suffix(body: &str, groups: &[Timed]) -> Vec<LyricsWord> {
    let mut words = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        let from = index
            .checked_sub(1)
            .and_then(|previous| groups.get(previous))
            .map(|previous| previous.close + 1)
            .unwrap_or(0);
        let text = entities::decode(body[from..group.open].trim());
        if text.is_empty() {
            continue;
        }
        words.push(LyricsWord {
            start: Duration::from_millis(group.start),
            end: Duration::from_millis(group.start.saturating_add(group.length)),
            text,
        });
    }
    words
}

fn words(body: &str, groups: &[Timed]) -> Vec<LyricsWord> {
    let leading = prefix(body, groups);
    let trailing = suffix(body, groups);
    let length = |words: &[LyricsWord]| -> usize {
        words.iter().map(|word| word.text.chars().count()).sum()
    };
    match length(&leading) >= length(&trailing) {
        true => leading,
        false => trailing,
    }
}

fn without_times(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(open) = rest.find('(') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find(')').map(|at| open + at) else {
            break;
        };
        let inner = &rest[open + 1..close];
        let timed = inner
            .split(',')
            .take(3)
            .all(|number| number.trim().parse::<u64>().is_ok());
        if !timed {
            out.push_str(&rest[open..=close]);
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn reads_a_wrapped_line_with_words() {
        let document = r#"<QrcInfos><LyricInfo><Lyric_1 LyricContent="[1000,2000]Hello (0,500)world (500,500)" /></LyricInfo></QrcInfos>"#;
        let lines = parse(document);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Hello world");
        let words = lines[0].words.as_deref().unwrap();
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "Hello");
        assert_eq!(words[1].start.as_millis(), 500);
    }

    #[test]
    fn reads_bare_lines_and_entities() {
        let lines = parse("[0,3000]rock &amp; roll");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "rock & roll");
    }
}
