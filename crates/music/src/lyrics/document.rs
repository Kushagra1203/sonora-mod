//! Reads the many shapes lyric services answer with.
//!
//! Everything from a bare LRC, a TTML document, an escaped XML attribute holding karaoke lines,
//! a JSON envelope around any of those, to plain prose makes its way through here.

use serde_json::Value;

use crate::Lyrics;
use crate::lyrics::{entities, karaoke, lrc, ttml};

/// Reads a provider's body into lyrics, or nothing when it holds no words.
pub(crate) fn read(body: &str) -> Option<Lyrics> {
    let text = body.trim().trim_start_matches('\u{feff}');
    if text.is_empty() {
        return None;
    }

    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return from_json(&value);
    }

    from_text(text)
}

fn from_json(value: &Value) -> Option<Lyrics> {
    match value {
        Value::String(text) => from_text(text),
        Value::Array(items) => {
            let text = items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n");
            from_text(&text)
        }
        Value::Object(object) => {
            // A service that says it failed did, whatever else the document holds.
            if object.get("ok").and_then(Value::as_bool) == Some(false)
                || object.get("isError").and_then(Value::as_bool) == Some(true)
            {
                return None;
            }
            for key in [
                "ttml",
                "ttmlContent",
                "lyrics",
                "lrc",
                "content",
                "text",
                "plain",
                "syncedLyrics",
                "plainLyrics",
                "data",
                "result",
                "response",
            ] {
                if let Some(found) = object.get(key).and_then(from_json) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

fn from_text(text: &str) -> Option<Lyrics> {
    let decoded = entities::decode(text);
    let trimmed = decoded.trim();
    if trimmed.is_empty() {
        return None;
    }

    if (trimmed.contains("<tt ") || trimmed.contains("<tt>"))
        && let Ok(sheet) = ttml::parse(trimmed)
        && !sheet.lyrics.is_empty()
    {
        return Some(sheet.lyrics);
    }

    if trimmed.contains("<QrcInfos") || trimmed.contains("LyricContent") {
        let lines = karaoke::parse(trimmed);
        if !lines.is_empty() {
            return Some(Lyrics::Synced {
                lines: lines.into(),
            });
        }
    }

    let lines = lrc::parse(trimmed);
    if !lines.is_empty() {
        return Some(Lyrics::Synced {
            lines: lines.into(),
        });
    }

    let plain = trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("[ti:") && !line.starts_with("[ar:"))
        .collect::<Vec<_>>()
        .join("\n");
    (!plain.is_empty()).then(|| Lyrics::plain(plain))
}

#[cfg(test)]
mod tests {
    use super::read;
    use crate::Lyrics;

    #[test]
    fn reads_a_ttml_wrapper() {
        let body = r#"{"ttml":"<tt xmlns=\"http://www.w3.org/ns/ttml\"><body><div><p begin=\"1s\" end=\"2s\">Hello</p></div></body></tt>"}"#;
        let lyrics = read(body).unwrap();
        assert!(matches!(lyrics, Lyrics::Synced { .. }));
    }

    #[test]
    fn reads_a_karaoke_wrapper() {
        let body = r#"{"lyrics":"<QrcInfos><LyricInfo><Lyric_1 LyricContent=\"[0,1000]Hello (0,500)world (500,500)\" /></LyricInfo></QrcInfos>"}"#;
        let lyrics = read(body).unwrap();
        assert!(lyrics.worded());
    }

    #[test]
    fn reads_plain_words() {
        let lyrics = read("One line\nAnother line").unwrap();
        assert!(matches!(lyrics, Lyrics::Plain { .. }));
    }

    #[test]
    fn ignores_a_failed_answer() {
        assert!(read(r#"{"ok":false,"message":"no"}"#).is_none());
    }

    #[test]
    fn ignores_an_empty_answer() {
        assert!(read(r#"{"lyrics":""}"#).is_none());
    }
}
