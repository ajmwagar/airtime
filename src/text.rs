//! TTS-safe text sanitization.
//!
//! LLMs (especially open-weights-via-OpenRouter) reflex into Markdown
//! even when told not to: `**bold**`, `*emphasis*`, bullet `- `, headings
//! with `#`, stage directions in `[brackets]`. Kokoro reads every one of
//! those characters literally — "asterisk asterisk donna asterisk
//! asterisk" — and the station instantly sounds wrong.
//!
//! `tts_safe` strips the markup before the script ever reaches the TTS
//! subprocess. Conservative on purpose: it removes characters that have
//! no business in spoken radio copy, normalises whitespace, and leaves
//! parens / em-dash / smart-quotes alone (those produce useful pauses).
//!
//! What gets stripped:
//! - `*`, `_`, `` ` ``, `#` (Markdown emphasis, headings, code)
//! - `[…]` and contents (stage directions like `[chuckles]`,
//!   `[laughs]`, or Markdown link wrappers)
//! - line-leading `-`, `*`, `>` (bullets / blockquotes)
//! - duplicate whitespace, collapsed to a single space
//!
//! What stays:
//! - `(…)` parens (TTS pauses naturally on them)
//! - `—` em-dash and `…` ellipsis (TTS pauses on them too)
//! - smart quotes, regular punctuation
//! - numerals (TTS pronounces them correctly)

/// Strip Markdown/stage-direction markup so the result is safe to feed
/// to a TTS engine. Idempotent; never lengthens its input.
pub fn tts_safe(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_bracket = false;
    let mut at_line_start = true;

    for ch in input.chars() {
        if in_bracket {
            if ch == ']' {
                in_bracket = false;
            }
            continue;
        }
        match ch {
            '[' => {
                in_bracket = true;
            }
            '*' | '_' | '`' | '#' => {}
            '\r' => {}
            '\n' => {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
                at_line_start = true;
            }
            '-' | '>' if at_line_start => {}
            c if c.is_whitespace() => {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
                at_line_start = false;
            }
            c => {
                out.push(c);
                at_line_start = false;
            }
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markdown_bold_and_italic() {
        assert_eq!(tts_safe("**hello** world"), "hello world");
        assert_eq!(tts_safe("*hello* world"), "hello world");
        assert_eq!(tts_safe("__bold__ and _italic_"), "bold and italic");
    }

    #[test]
    fn strips_inline_code_and_headings() {
        assert_eq!(tts_safe("# Heading\nBody"), "Heading Body");
        assert_eq!(tts_safe("use the `pump` function"), "use the pump function");
    }

    #[test]
    fn strips_stage_directions_in_brackets() {
        assert_eq!(
            tts_safe("Hey there [chuckles] welcome to the show"),
            "Hey there  welcome to the show"
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        );
        // The above normalises the double-space; just assert the bracket
        // content is gone and the words are present:
        let cleaned = tts_safe("Hey there [chuckles] welcome to the show");
        assert!(cleaned.contains("Hey there"));
        assert!(cleaned.contains("welcome to the show"));
        assert!(!cleaned.contains("chuckles"));
        assert!(!cleaned.contains('['));
        assert!(!cleaned.contains(']'));
    }

    #[test]
    fn strips_bullet_markers_at_line_start() {
        let input = "- first item\n- second item\n* third\n> quoted";
        let out = tts_safe(input);
        assert!(!out.contains("- "));
        assert!(!out.contains("* "));
        assert!(!out.contains("> "));
        assert!(out.contains("first item"));
        assert!(out.contains("second item"));
        assert!(out.contains("third"));
        assert!(out.contains("quoted"));
    }

    #[test]
    fn keeps_parens_and_em_dash_and_smart_quotes() {
        // These cue useful TTS pauses; don't strip.
        let cleaned = tts_safe("That was Miles (1959) — a classic. \u{201C}Cool\u{201D} cat.");
        assert!(cleaned.contains("(1959)"));
        assert!(cleaned.contains("—"));
        assert!(cleaned.contains("\u{201C}Cool\u{201D}"));
    }

    #[test]
    fn collapses_whitespace() {
        assert_eq!(tts_safe("hello   world\n\n\nagain"), "hello world again");
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert_eq!(tts_safe(""), "");
        assert_eq!(tts_safe("   \n\n  "), "");
    }

    #[test]
    fn idempotent_on_already_clean_text() {
        let input = "Good evening Seattle. This is Donna on KFLT.";
        assert_eq!(tts_safe(input), input);
        assert_eq!(tts_safe(&tts_safe(input)), input);
    }

    #[test]
    fn handles_typical_llm_output() {
        let raw = "**Welcome back!** Coming up next: *Kind of Blue* by Miles Davis. \
                   [Note: 1959 Columbia] A real classic. \n\n\
                   - Track 1: So What\n\
                   - Track 2: Freddie Freeloader";
        let cleaned = tts_safe(raw);
        assert!(!cleaned.contains('*'));
        assert!(!cleaned.contains('['));
        assert!(!cleaned.contains(']'));
        assert!(!cleaned.contains("- "));
        assert!(cleaned.contains("Welcome back"));
        assert!(cleaned.contains("Kind of Blue"));
        assert!(cleaned.contains("Track 1: So What"));
    }
}
