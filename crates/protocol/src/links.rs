//! Shared link parsing. Never treat a URL-shaped string as permission to fetch.
use crate::{LinkDestination, MessageLink};
use percent_encoding::percent_decode_str;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

fn invalid_text(s: &str) -> bool {
    s.chars().any(|c| c.is_control() || c == '\u{fffd}' || matches!(c, '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
}

pub fn classify(raw: &str) -> LinkDestination {
    let reject = |reason: &str| LinkDestination::Unsupported {
        reason: reason.into(),
    };
    if raw.is_empty() || raw.len() > 8192 || raw.trim() != raw || invalid_text(raw) {
        return reject("Empty, oversized, or suspicious characters in destination");
    }
    let bytes = raw.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'%'
            && (i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit())
        {
            return reject("Malformed percent encoding");
        }
    }
    let decoded = match percent_decode_str(raw).decode_utf8() {
        Ok(value) if !invalid_text(&value) => value,
        _ => return reject("Invalid or suspicious encoded characters"),
    };
    if raw.starts_with('#') || raw.starts_with("//") || raw.contains('\\') {
        return reject("Ambiguous destination; use an explicit URL or file path");
    }
    match url::Url::parse(raw) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => {
            if !raw
                .to_ascii_lowercase()
                .starts_with(&format!("{}://", url.scheme()))
            {
                return reject("Web URLs must include // after the scheme");
            }
            if !url.username().is_empty() || url.password().is_some() {
                return reject("Embedded credentials are not supported");
            }
            let Some(host) = url.host_str() else {
                return reject("Missing hostname");
            };
            let mut warnings = Vec::new();
            if host.split('.').any(|part| part.starts_with("xn--")) {
                warnings.push("Internationalized hostname shown as ASCII/Punycode".into());
            }
            if url.as_str() != raw {
                warnings.push(
                    "URL spelling was normalized; compare the original destination below".into(),
                );
            }
            LinkDestination::Web {
                url: url.to_string(),
                host: host.into(),
                warnings,
            }
        }
        Ok(url)
            if url.scheme() == "file"
                && url.host_str().is_none_or(|h| h == "localhost")
                && url.query().is_none()
                && url.fragment().is_none() =>
        {
            LinkDestination::File {
                path: percent_decode_str(url.path())
                    .decode_utf8_lossy()
                    .into_owned(),
            }
        }
        Ok(_) => reject("Only HTTP(S) URLs and local file paths are supported"),
        Err(url::ParseError::RelativeUrlWithoutBase) => LinkDestination::File {
            path: decoded.into_owned(),
        },
        Err(_) => reject("Malformed URL"),
    }
}

pub fn extract(source: &str) -> Vec<MessageLink> {
    let mut links: Vec<MessageLink> = Vec::new();
    let mut current: Option<(String, String)> = None;
    for event in Parser::new_ext(
        source,
        Options::ENABLE_TABLES
            | Options::ENABLE_MATH
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS,
    ) {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                current = Some((dest_url.into_string(), String::new()))
            }
            Event::Text(t) | Event::Code(t) if current.is_some() => {
                current.as_mut().unwrap().1.push_str(&t)
            }
            Event::End(TagEnd::Link) => {
                if let Some((destination, title)) = current.take()
                    && !links.iter().any(|l| l.destination == destination)
                {
                    let kind = classify(&destination);
                    links.push(MessageLink {
                        title: if title.is_empty() {
                            destination.clone()
                        } else {
                            title
                        },
                        destination,
                        kind,
                    });
                }
            }
            _ => {}
        }
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normal_urls_paths_and_deduplication() {
        assert!(matches!(
            classify("https://example.org/a?q=a+b&x=1#part"),
            LinkDestination::Web { .. }
        ));
        assert!(matches!(
            classify("https://[::1]:8443/a"),
            LinkDestination::Web { .. }
        ));
        assert_eq!(
            classify("/tmp/a%20b.json"),
            LinkDestination::File {
                path: "/tmp/a b.json".into()
            }
        );
        let links =
            extract("[One](/tmp/a) and [Again](/tmp/a) ` [Code](/tmp/b) ` ![Image](/tmp/c)");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].title, "One");
    }
    #[test]
    fn ambiguous_or_invalid_destinations_are_explicit() {
        for value in [
            "https://example.org/\nfoo",
            "https://example.org/%0a",
            "https://example.org/%GG",
            "https://example.org/�",
            "https://user:pass@example.org",
            "mailto:a@example.org",
            "https:example.org",
            "//example.org/a",
        ] {
            assert!(
                matches!(classify(value), LinkDestination::Unsupported { .. }),
                "{value}"
            );
        }
    }
}
