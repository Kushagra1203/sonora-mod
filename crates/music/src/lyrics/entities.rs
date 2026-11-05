//! The few HTML/XML entities lyric pages actually use.
//!
//! Provider documents arrive as HTML, XML attributes and JSON strings that have already been
//! escaped once or twice. Rather than pull a whole parser in for a short list of names, decode
//! the ones lyrics pages use and leave anything unknown as it was.

/// Replaces named and numeric entities in `text`.
pub(crate) fn decode(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }

    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let Some(end) = after.find(';').filter(|end| *end <= 12) else {
            out.push('&');
            rest = &after[1..];
            continue;
        };

        let name = &after[1..end];
        match name_from(name) {
            Some(decoded) => {
                out.push_str(&decoded);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn name_from(name: &str) -> Option<String> {
    let named = match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => " ",
        "hellip" => "…",
        "mdash" => "—",
        "ndash" => "–",
        "lsquo" => "‘",
        "rsquo" => "’",
        "ldquo" => "“",
        "rdquo" => "”",
        "laquo" => "«",
        "raquo" => "»",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "deg" => "°",
        "middot" => "·",
        "bull" => "•",
        _ => return numeric(name),
    };
    Some(named.to_owned())
}

fn numeric(name: &str) -> Option<String> {
    let digits = name.strip_prefix('#')?;
    let code = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse().ok()?,
    };
    char::from_u32(code).map(|character| character.to_string())
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn decodes_named_and_numeric_entities() {
        assert_eq!(decode("rock &amp; roll"), "rock & roll");
        assert_eq!(decode("&#39;til &#x1F600;"), "'til 😀");
        assert_eq!(decode("no &unknown; change"), "no &unknown; change");
    }
}
