//! HTML sanitization for incoming ActivityPub content.
//!
//! Remote Note content arrives as HTML. We sanitize it with ammonia to a
//! conservative allowlist before storing or serving it.
//!
//! Allowed tags mirror Mitra's list (more conservative than GoToSocial) with a
//! few additions to support common Markdown-rendered output (ul/ol/li, etc.),
//! since many fediverse clients render Markdown → HTML before sending.

use ammonia::Builder;
use std::collections::HashSet;

/// Sanitize HTML from a remote Note's `content` field.
/// Returns clean, safe HTML suitable for embedding in blog pages.
pub fn sanitize_note_html(html: &str) -> String {
    Builder::default()
        .tags(allowed_tags())
        // Allow `class` only on these specific elements (for Mastodon mention/hashtag markup)
        .tag_attributes(tag_attributes())
        // All links get rel="nofollow noreferrer" and target="_blank"
        .link_rel(Some("nofollow noreferrer noopener"))
        .clean(html)
        .to_string()
}

/// Sanitize a plain-text field (name, summary) — strip all HTML.
#[allow(dead_code)]
fn strip_html(html: &str) -> String {
    ammonia::Builder::empty().clean(html).to_string()
}

fn allowed_tags() -> HashSet<&'static str> {
    [
        // Block-level
        "p", "br", "blockquote", "pre", "code",
        // Inline formatting
        "strong", "em", "b", "i", "u", "s", "del", "ins", "strike",
        "sub", "sup", "mark", "small",
        // Links
        "a",
        // Headings (rendered from Markdown)
        "h1", "h2", "h3", "h4", "h5", "h6",
        // Lists
        "ul", "ol", "li",
        // Spans (needed for Mastodon mention/hashtag h-card)
        "span",
        // Tables (rendered from Markdown)
        "table", "thead", "tbody", "tr", "th", "td",
    ]
    .into()
}

fn tag_attributes() -> std::collections::HashMap<&'static str, HashSet<&'static str>> {
    let mut map = std::collections::HashMap::new();
    // Allow href on <a>; class for Mastodon mention/hashtag markup.
    // Note: `rel` is managed by `link_rel()` — don't include it here.
    map.insert("a", ["href", "class", "title"].into());
    // Allow class on span for Mastodon's h-card spans
    map.insert("span", ["class"].into());
    // Allow class on code for syntax highlighting
    map.insert("code", ["class"].into());
    // Allow lang on blockquote (sometimes used for quotes)
    map.insert("blockquote", ["cite"].into());
    // Table alignment
    map.insert("th", ["align", "scope"].into());
    map.insert("td", ["align"].into());
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_script_tags() {
        let input = r#"<p>Hello</p><script>alert('xss')</script>"#;
        let out = sanitize_note_html(input);
        assert!(!out.contains("script"));
        assert!(out.contains("Hello"));
    }

    #[test]
    fn preserves_mastodon_mention_class() {
        let input = r#"<span class="h-card"><a class="u-url mention" href="https://mastodon.social/@alice">@alice</a></span>"#;
        let out = sanitize_note_html(input);
        assert!(out.contains("h-card"));
    }

    #[test]
    fn forces_nofollow_on_links() {
        let input = r#"<a href="https://example.com" rel="me">link</a>"#;
        let out = sanitize_note_html(input);
        assert!(out.contains("nofollow"));
    }

    #[test]
    fn strip_html_removes_all_tags() {
        let input = "<p>Hello <strong>world</strong></p>";
        assert_eq!(strip_html(input), "Hello world");
    }
}
