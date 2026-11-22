//! Display trim for daemon text and ssh stderr.
//!
//! Only external text goes through [`display_text`]; Combine's own messages are published whole.

pub const DISPLAY_TEXT_MAX_CHARS: usize = 256;
pub const TRUNCATION_MARK: char = '\u{2026}';
pub const EMPTY_TEXT: &str = "(no message)";

fn is_separator(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(
            c,
            '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
        )
}

/// True when every char is a separator, so the text shows as nothing.
pub fn is_blank(s: &str) -> bool {
    s.chars().all(is_separator)
}

/// Makes external text safe to show on one line.
///
/// Streams over the input and stops as soon as the limit is reached, so a huge input costs
/// at most `DISPLAY_TEXT_MAX_CHARS` chars of work after the cut. Each run of separators
/// becomes one space, leading and trailing runs vanish, and a cut ends in
/// [`TRUNCATION_MARK`], so the result is at most 257 chars. An empty result is
/// [`EMPTY_TEXT`]. Applying it to its own output changes nothing.
///
/// ```text
/// display_text("ab\n\n\tcd")  ==  "ab cd"
/// ```
pub fn display_text(raw: &str) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    let mut pending_space = false;
    let mut truncated = false;
    for c in raw.chars() {
        if is_separator(c) {
            pending_space = count > 0;
            continue;
        }
        let needed = 1 + usize::from(pending_space);
        if count + needed > DISPLAY_TEXT_MAX_CHARS {
            truncated = true;
            break;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
        count += needed;
    }
    if truncated {
        out.push(TRUNCATION_MARK);
    }
    if out.is_empty() {
        EMPTY_TEXT.to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_one_mebibyte_text_to_limit_with_mark() {
        let out = display_text(&"x".repeat(1 << 20));
        assert_eq!(out, format!("{}\u{2026}", "x".repeat(256)));
        assert_eq!(out.chars().count(), 257);
    }

    #[test]
    fn strips_controls_and_collapses_whitespace() {
        assert_eq!(display_text("ab\n\n\tcd\u{1b}[0m  "), "ab cd [0m");
    }

    #[test]
    fn strips_bidi_controls() {
        let raw = "a\u{061C}b\u{200E}c\u{200F}d\u{202A}e\u{202E}f\u{2066}g\u{2069}h";
        assert_eq!(display_text(raw), "a b c d e f g h");
        assert_eq!(display_text("\u{202D}\u{2067}"), EMPTY_TEXT);
    }

    #[test]
    fn short_text_unchanged() {
        assert_eq!(display_text("connection closed"), "connection closed");
    }

    #[test]
    fn empty_becomes_placeholder() {
        assert_eq!(display_text(""), EMPTY_TEXT);
        assert_eq!(display_text(" \n\t\u{1b}"), EMPTY_TEXT);
    }

    #[test]
    fn is_idempotent() {
        let inputs = [
            "x".repeat(1 << 20),
            format!("{} {}", "y".repeat(255), "z".repeat(10)),
            "ab\n\n\tcd\u{1b}[0m  ".to_string(),
            String::new(),
        ];
        for raw in &inputs {
            let once = display_text(raw);
            assert_eq!(display_text(&once), once);
        }
    }

    #[test]
    fn is_blank_cases() {
        assert!(is_blank(""));
        assert!(is_blank(" \n\t\u{1b}\u{200E}"));
        assert!(!is_blank(" a "));
        assert!(!is_blank(EMPTY_TEXT));
    }
}
