//! Deterministic post-transcription find/replace, mirroring SuperWhisper's
//! "Replacements". The user maps a spoken phrase to literal text ("at sign" -> "@",
//! "my email" -> "me@example.com") and it is applied to the whisper output verbatim.
//!
//! This is deliberately *not* AI-mediated: a model asked to "substitute these phrases"
//! will also silently reword, re-punctuate, or refuse, and it costs a network round
//! trip on every dictation. Replacements must be predictable enough that a user can
//! rely on them for email addresses and command names, so they run here, offline,
//! before any AI mode gets the text.

use super::settings::Replacement;

/// Apply `rules` to `text` in a single left-to-right pass.
///
/// Matching is ASCII-case-insensitive and bounded: a phrase only matches when the
/// characters on either side are non-alphanumeric (or it sits at the start/end of the
/// input), so a rule for "at" leaves "battle" and "that" alone.
///
/// At each input position the **longest matching phrase wins**, regardless of the
/// order the rules were entered in. This matters because rule order is invisible in
/// the UI: someone who already has "at sign" -> "@" and later adds "at" -> "@" would
/// otherwise silently break the first rule, with no way to see why. Ties between
/// equal-length phrases fall back to the order the user listed them, so the result is
/// still fully deterministic.
///
/// Replacement output is never re-scanned, so rules cannot cascade into each other
/// ("a" -> "b" followed by "b" -> "c" yields "b", not "c").
///
/// Rules whose `from` is empty or whitespace-only are skipped. An empty `to` simply
/// deletes the phrase; no whitespace tidying is done.
pub fn apply_replacements(text: &str, rules: &[Replacement]) -> String {
    let mut active: Vec<&Replacement> =
        rules.iter().filter(|r| !r.from.trim().is_empty()).collect();
    // Stable sort: longest phrase first, user order preserved within a length.
    active.sort_by(|a, b| b.from.len().cmp(&a.from.len()));
    if active.is_empty() {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < text.len() {
        let mut matched = None;
        if !prev_char_is_alphanumeric(text, i) {
            for rule in &active {
                let end = i + rule.from.len();
                if end <= text.len()
                    && bytes[i..end].eq_ignore_ascii_case(rule.from.as_bytes())
                    && !next_char_is_alphanumeric(text, end)
                {
                    matched = Some((rule, end));
                    break;
                }
            }
        }

        match matched {
            Some((rule, end)) => {
                out.push_str(&rule.to);
                i = end;
            }
            None => {
                // Advance a whole char so multi-byte input is copied intact.
                let c = text[i..].chars().next().expect("i is a char boundary");
                out.push(c);
                i += c.len_utf8();
            }
        }
    }

    out
}

fn prev_char_is_alphanumeric(text: &str, at: usize) -> bool {
    text[..at]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric())
}

fn next_char_is_alphanumeric(text: &str, at: usize) -> bool {
    text[at..]
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(from: &str, to: &str) -> Replacement {
        Replacement {
            from: from.to_string(),
            to: to.to_string(),
        }
    }

    #[test]
    fn replaces_a_single_word() {
        let rules = [rule("hey", "hi")];
        assert_eq!(apply_replacements("hey there", &rules), "hi there");
    }

    #[test]
    fn replaces_a_multi_word_phrase() {
        let rules = [rule("at sign", "@")];
        assert_eq!(
            apply_replacements("me at sign example dot com", &rules),
            "me @ example dot com"
        );
    }

    #[test]
    fn matching_ignores_case_in_both_directions() {
        let rules = [rule("At Sign", "@")];
        assert_eq!(apply_replacements("AT SIGN", &rules), "@");
        assert_eq!(apply_replacements("at sign", &rules), "@");
        let lower = [rule("my email", "me@example.com")];
        assert_eq!(
            apply_replacements("My Email please", &lower),
            "me@example.com please"
        );
    }

    #[test]
    fn does_not_match_inside_a_longer_word() {
        let rules = [rule("at", "@")];
        assert_eq!(
            apply_replacements("battle that format", &rules),
            "battle that format",
            "a bounded rule must never touch the interior of a word"
        );
    }

    #[test]
    fn matches_at_string_start_and_end() {
        let rules = [rule("at", "@")];
        assert_eq!(apply_replacements("at", &rules), "@");
        assert_eq!(apply_replacements("at home", &rules), "@ home");
        assert_eq!(apply_replacements("look at", &rules), "look @");
    }

    #[test]
    fn punctuation_counts_as_a_boundary() {
        let rules = [rule("at sign", "@")];
        assert_eq!(apply_replacements("at sign.", &rules), "@.");
        assert_eq!(apply_replacements("(at sign)", &rules), "(@)");
        assert_eq!(apply_replacements("say \"at sign\"", &rules), "say \"@\"");
    }

    #[test]
    fn output_of_one_rule_is_not_rescanned_by_a_later_rule() {
        let rules = [rule("a", "b"), rule("b", "c")];
        assert_eq!(
            apply_replacements("a b", &rules),
            "b c",
            "each input position is consumed once; rules must not cascade"
        );
    }

    #[test]
    fn longest_phrase_wins_regardless_of_rule_order() {
        // The shorter rule is listed first, and must still lose: a user who adds
        // "at" later must not silently break their existing "at sign" rule.
        let rules = [rule("at", "@"), rule("at sign", "AT-SIGN")];
        assert_eq!(apply_replacements("at sign", &rules), "AT-SIGN");
        // Same rules, reversed input order -> same answer.
        let reversed = [rule("at sign", "AT-SIGN"), rule("at", "@")];
        assert_eq!(apply_replacements("at sign", &reversed), "AT-SIGN");
        // The shorter rule still applies where the longer one does not match.
        assert_eq!(apply_replacements("look at that", &rules), "look @ that");
    }

    #[test]
    fn equal_length_phrases_fall_back_to_user_order() {
        let rules = [rule("cat", "FIRST"), rule("cat", "SECOND")];
        assert_eq!(
            apply_replacements("cat", &rules),
            "FIRST",
            "ties must resolve deterministically to the earlier rule"
        );
    }

    #[test]
    fn empty_rule_list_is_a_no_op() {
        assert_eq!(apply_replacements("nothing changes", &[]), "nothing changes");
    }

    #[test]
    fn empty_and_whitespace_from_rules_are_skipped() {
        let rules = [rule("", "BOOM"), rule("   ", "BOOM"), rule("ok", "fine")];
        assert_eq!(
            apply_replacements("ok then", &rules),
            "fine then",
            "blank patterns must neither match nor stall the scan"
        );
    }

    #[test]
    fn empty_to_deletes_the_phrase_without_tidying_whitespace() {
        let rules = [rule("um", "")];
        assert_eq!(
            apply_replacements("so um yeah", &rules),
            "so  yeah",
            "deletion is literal; the double space is intentional"
        );
    }

    #[test]
    fn preserves_newlines_and_surrounding_punctuation() {
        let rules = [rule("new line", "\n")];
        assert_eq!(
            apply_replacements("one\ntwo new line three", &rules),
            "one\ntwo \n three"
        );
    }

    #[test]
    fn unicode_input_passes_through_uncorrupted() {
        let rules = [rule("cafe", "café")];
        assert_eq!(
            apply_replacements("naïve café — 日本語 🎤 cafe", &rules),
            "naïve café — 日本語 🎤 café"
        );
    }

    #[test]
    fn non_ascii_letters_act_as_word_characters() {
        let rules = [rule("na", "N"), rule("ve", "V")];
        assert_eq!(
            apply_replacements("naïve", &rules),
            "naïve",
            "a match must not start or end against a non-ASCII letter"
        );
    }

    #[test]
    fn empty_input_stays_empty() {
        let rules = [rule("at", "@")];
        assert_eq!(apply_replacements("", &rules), "");
    }
}
