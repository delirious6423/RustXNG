//! Readable text extraction for the optional raw-content results.
//!
//! This is not a full readability implementation: it takes the first nonempty
//! article/main/body candidate and normalises the whitespace, which is enough
//! for an agent to read the page behind a search result.

use std::sync::LazyLock;

use scraper::{Html, Selector};

/// Longest text returned per page, in characters, unless the caller asks for
/// a different limit.
const MAX_CHARS: usize = 4_000;

static CANDIDATES: LazyLock<[Selector; 4]> = LazyLock::new(|| {
    ["article", "main", "[role=main]", "body"]
        .map(|selector| Selector::parse(selector).expect("valid built-in content selector"))
});

/// The readable text of a parsed HTML document.
pub fn main_text(html: &Html) -> String {
    main_text_limited(html, MAX_CHARS)
}

/// The readable text of a parsed HTML document, capped at the given length.
pub fn main_text_limited(html: &Html, max: usize) -> String {
    for selector in CANDIDATES.iter() {
        if let Some(element) = html.select(selector).next() {
            let text = normalise_limited(element.text(), max);
            if !text.is_empty() {
                return text;
            }
        }
    }
    String::new()
}

/// Normalize only as far as the retained prefix and one additional character.
///
/// A pending separator preserves both `text_nodes.join(" ")` and
/// `split_whitespace().join(" ")` without allocating either full-page string.
/// It is emitted only when another non-whitespace character follows, so a
/// trailing whitespace-only tail never adds an ellipsis by itself.
fn normalise_limited<'a>(text_nodes: impl Iterator<Item = &'a str>, max: usize) -> String {
    let mut out = String::new();
    let mut chars = 0;
    let mut separator = false;
    for node in text_nodes {
        // Character iteration also stops early inside a single long token;
        // split_whitespace would first scan the whole token to find its end.
        for character in node.chars() {
            if character.is_whitespace() {
                separator = chars > 0;
                continue;
            }
            if separator {
                if chars == max {
                    out.push('…');
                    return out;
                }
                out.push(' ');
                chars += 1;
                separator = false;
            }
            if chars == max {
                out.push('…');
                return out;
            }
            out.push(character);
            chars += 1;
        }
        // Joining adjacent DOM text nodes inserts a space even when the
        // original markup had none, e.g. <b>one</b><i>two</i>.
        separator = chars > 0;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn article_wins_over_body() {
        let html = Html::parse_document(
            "<html><body><nav>menu</nav><article><p>Hello   world</p></article></body></html>",
        );
        assert_eq!(main_text(&html), "Hello world");
    }

    #[test]
    fn plain_body_is_the_fallback() {
        let html = Html::parse_document("<html><body><p>Just text</p></body></html>");
        assert_eq!(main_text(&html), "Just text");
    }

    #[test]
    fn long_text_is_truncated() {
        let body = "a".repeat(MAX_CHARS + 100);
        let html = Html::parse_document(&format!("<html><body><p>{body}</p></body></html>"));
        let text = main_text(&html);
        assert!(text.chars().count() <= MAX_CHARS + 1);
    }

    /// The pre-optimization implementation, kept independent of the streaming
    /// helper so these tests compare complete output bytes against its behavior.
    fn reference_main_text_limited(html: &Html, max: usize) -> String {
        for selector in ["article", "main", "[role=main]", "body"] {
            let selector = Selector::parse(selector).unwrap();
            if let Some(element) = html.select(&selector).next() {
                let joined = element.text().collect::<Vec<_>>().join(" ");
                let text = joined.split_whitespace().collect::<Vec<_>>().join(" ");
                if !text.is_empty() {
                    if text.chars().count() <= max {
                        return text;
                    }
                    let mut prefix: String = text.chars().take(max).collect();
                    prefix.push('…');
                    return prefix;
                }
            }
        }
        String::new()
    }

    #[test]
    fn streaming_matches_reference_at_unicode_and_container_boundaries() {
        let cases = [
            "<body> \t\n </body>",
            "<body><b>one</b><i>two</i><span> </span>three</body>",
            "<body><article> </article><article>ignored second article</article><main>chosen</main></body>",
            "<body><article> </article><main> </main><div role=main>chosen</div>body</body>",
            "<body><article>chosen</article><main>ignored</main>body</body>",
            "<body><article> </article><main> </main><div role=main> </div>fallback</body>",
            "<body><article>é中😀e\u{301}\u{200b}人</article></body>",
            "<body><article>ab <span> cd </span> ef </article></body>",
            "<body><article>a\u{85}\u{a0}\u{1680}\u{2000}\u{2001}\u{2002}\u{2003}\u{2004}\u{2005}\u{2006}\u{2007}\u{2008}\u{2009}\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}b</article></body>",
            "<body><article>&amp; &nbsp; &lt; &#128512; &#x4e2d;</article></body>",
            "<body><article><script>var x = 1;</script><style>.x {color: red;}</style>text</article></body>",
        ];
        for source in cases {
            let html = Html::parse_document(source);
            let length = reference_main_text_limited(&html, usize::MAX)
                .chars()
                .count();
            for max in (0..=length + 1).chain([usize::MAX]) {
                assert_eq!(
                    main_text_limited(&html, max),
                    reference_main_text_limited(&html, max),
                    "max={max}, source={source:?}"
                );
            }
        }
    }

    #[test]
    fn streaming_matches_reference_for_generated_text_nodes() {
        // Fixed-seed, dependency-free generation exercises different text-node
        // boundaries, Unicode widths, whitespace runs, and truncation points.
        let alphabet = [
            'a', 'Z', '中', 'é', '😀', '\u{301}', '\u{200b}', ' ', '\t', '\n', '\r', '\u{85}',
            '\u{a0}', '\u{2003}', '\u{3000}',
        ];
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        for case in 0..128 {
            let mut source = String::from("<body><article>");
            for _ in 0..12 {
                source.push_str("<span>");
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let length = (state >> 32) as usize % 25;
                for _ in 0..length {
                    state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                    source.push(alphabet[(state >> 32) as usize % alphabet.len()]);
                }
                source.push_str("</span>");
            }
            source.push_str("</article></body>");
            let html = Html::parse_document(&source);
            for max in [0, 1, 2, 3, 7, 15, 31, 63, 127, 255, usize::MAX] {
                assert_eq!(
                    main_text_limited(&html, max),
                    reference_main_text_limited(&html, max),
                    "case={case}, max={max}, source={source:?}"
                );
            }
        }
    }

    #[test]
    fn streaming_does_not_visit_nodes_past_the_truncation_point() {
        let nodes = std::iter::once("abcdef").chain(std::iter::once_with(|| {
            panic!("text past the first omitted character must not be visited")
        }));
        assert_eq!(normalise_limited(nodes, 3), "abc…");
    }

    #[test]
    fn exact_limit_ignores_a_trailing_whitespace_only_tail() {
        assert_eq!(
            normalise_limited(["abc", " \t\n", ""].into_iter(), 3),
            "abc"
        );
        assert_eq!(normalise_limited(["a b"].into_iter(), 2), "a …");
        assert_eq!(normalise_limited([" \n "].into_iter(), 0), "");
        assert_eq!(normalise_limited([" \n a"].into_iter(), 0), "…");
    }
}
