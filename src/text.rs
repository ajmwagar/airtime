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

/// Wrap known names in Kokoro's IPA-override syntax so the misaki
/// tokenizer pronounces them right. Format: `[Name](/IPA/)` — misaki
/// strips the bracketed display word and uses the IPA in parens.
///
/// Whole-word, case-insensitive matching; longest keys win (so "Miles
/// Davis" matches before "Miles"). Must run AFTER `tts_safe`, since
/// `tts_safe` strips brackets and would eat our markers. The dict
/// comes from `[host.pronunciations]` in the persona TOML.
///
/// Empty dict → no-op string clone. Names containing characters other
/// than letters / spaces / apostrophes / hyphens are skipped — the
/// boundary scan can't safely place them and they'd usually be
/// spell-checker artefacts anyway.
///
/// Single-pass scanner: at each character position, try the longest
/// matching key; on match, emit the wrap and advance past it (so a
/// subsequent short key can't re-match inside the wrap). On no match,
/// copy the char and advance.
pub fn apply_pronunciations(
    input: &str,
    dict: &std::collections::HashMap<String, String>,
) -> String {
    if dict.is_empty() {
        return input.to_string();
    }
    let lower_input = input.to_lowercase();
    // Byte-offset arithmetic requires parallel byte positions; bail
    // if a Unicode case fold changed the byte length (rare — German ß).
    if lower_input.len() != input.len() {
        return input.to_string();
    }
    // Pre-sort by lowercased byte length, longest first.
    let mut keys: Vec<(&String, &String, String)> = dict
        .iter()
        .filter(|(k, _)| !k.is_empty() && is_safe_key(k))
        .map(|(k, v)| (k, v, k.to_lowercase()))
        .collect();
    keys.sort_by_key(|(_, _, lower_k)| std::cmp::Reverse(lower_k.len()));

    let bytes = input.as_bytes();
    let lower_bytes = lower_input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        let before_is_word = input[..i]
            .chars()
            .next_back()
            .map(|c| c.is_alphanumeric())
            .unwrap_or(false);
        let mut matched = None;
        if !before_is_word {
            for (orig, ipa, lower_k) in &keys {
                let len = lower_k.len();
                if i + len > bytes.len() {
                    continue;
                }
                if &lower_bytes[i..i + len] != lower_k.as_bytes() {
                    continue;
                }
                let after_is_word = input[i + len..]
                    .chars()
                    .next()
                    .map(|c| c.is_alphanumeric())
                    .unwrap_or(false);
                if !after_is_word {
                    matched = Some((orig, ipa, len));
                    break;
                }
            }
        }
        if let Some((key, ipa, len)) = matched {
            out.push_str(&format!("[{key}](/{ipa}/)"));
            i += len;
        } else {
            // Walk one char so we don't split multi-byte sequences.
            let next_char = input[i..].chars().next();
            if let Some(c) = next_char {
                out.push(c);
                i += c.len_utf8();
            } else {
                break;
            }
        }
    }
    out
}

fn is_safe_key(k: &str) -> bool {
    k.chars()
        .all(|c| c.is_alphabetic() || c == ' ' || c == '\'' || c == '-')
}

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

    fn dict(entries: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn pronunciations_empty_dict_is_noop() {
        let d = std::collections::HashMap::new();
        assert_eq!(
            apply_pronunciations("Asake is up next", &d),
            "Asake is up next"
        );
    }

    #[test]
    fn pronunciations_wraps_with_ipa_override() {
        let d = dict(&[("Asake", "əˈsɑːkeɪ")]);
        let out = apply_pronunciations("Coming up — Asake.", &d);
        assert_eq!(out, "Coming up — [Asake](/əˈsɑːkeɪ/).");
    }

    #[test]
    fn pronunciations_case_insensitive_match() {
        let d = dict(&[("Asake", "əˈsɑːkeɪ")]);
        let out = apply_pronunciations("here's asake again", &d);
        assert!(out.contains("[Asake](/əˈsɑːkeɪ/)"));
    }

    #[test]
    fn pronunciations_respects_word_boundaries() {
        // "Asakelike" shouldn't match the "Asake" entry.
        let d = dict(&[("Asake", "əˈsɑːkeɪ")]);
        let out = apply_pronunciations("Asakelike is not a word", &d);
        assert_eq!(out, "Asakelike is not a word");
    }

    #[test]
    fn pronunciations_longest_match_wins() {
        // "Miles Davis" must be wrapped as a unit, not as "Miles" + "Davis".
        let d = dict(&[("Miles", "maɪlz"), ("Miles Davis", "maɪlz ˈdeɪvɪs")]);
        let out = apply_pronunciations("Miles Davis is on", &d);
        assert!(
            out.contains("[Miles Davis](/maɪlz ˈdeɪvɪs/)"),
            "long form should win: {out}"
        );
        assert!(
            !out.contains("[Miles](/maɪlz/) Davis"),
            "short form must not also match: {out}"
        );
    }

    #[test]
    fn pronunciations_multiple_entries_in_one_pass() {
        let d = dict(&[("Asake", "əˈsɑːkeɪ"), ("Burna Boy", "ˈbɜːnə bɔɪ")]);
        let out = apply_pronunciations("Asake follows Burna Boy.", &d);
        assert!(out.contains("[Asake](/əˈsɑːkeɪ/)"));
        assert!(out.contains("[Burna Boy](/ˈbɜːnə bɔɪ/)"));
    }

    #[test]
    fn pronunciations_skips_keys_with_unsafe_chars() {
        // Keys with brackets/parens would break the syntax — silently skip.
        let d = dict(&[("Bad[Key]", "x"), ("Asake", "əˈsɑːkeɪ")]);
        let out = apply_pronunciations("Asake is fine", &d);
        assert!(out.contains("[Asake](/əˈsɑːkeɪ/)"));
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
