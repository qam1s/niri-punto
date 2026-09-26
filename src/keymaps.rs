//! Minimal US/RU character tables for selection conversion.
//!
//! The scancode-replay path (tickets 08/10) needs no tables: it re-emits the
//! same scancodes after the layout switches. The clipboard path of ticket 09
//! cannot replay — the selection may be any Unicode the daemon never typed —
//! so it maps the pasted text char-by-char to the other layout instead.
//!
//! Hardcoded assumption (shared with ticket 08): the pair is US QWERTY at
//! niri index 0 and Russian JCUKEN at index 1. A configured pair (ticket 11)
//! will replace these tables; until then anything outside them passes
//! through unchanged, so emoji and third-language text survive the round
//! trip byte-identical.
//!
//! [`convert`] maps both directions: `from_ru == false` is EN->RU (current
//! layout is US), `from_ru == true` is RU->EN.

/// One bidirectional character pair: US QWERTY position <-> RU JCUKEN glyph.
///
/// Lowercase letters and unshifted punctuation are listed; uppercase letters
/// derive via case folding (Unicode-aware, 1:1 for Cyrillic). Shifted symbols
/// that have no case relation (`{`, `<`, `@`, …) are listed explicitly.
const PAIRS: &[(char, char)] = &[
    // Lowercase letters, row by row.
    ('q', 'й'),
    ('w', 'ц'),
    ('e', 'у'),
    ('r', 'к'),
    ('t', 'е'),
    ('y', 'н'),
    ('u', 'г'),
    ('i', 'ш'),
    ('o', 'щ'),
    ('p', 'з'),
    ('a', 'ф'),
    ('s', 'ы'),
    ('d', 'в'),
    ('f', 'а'),
    ('g', 'п'),
    ('h', 'р'),
    ('j', 'о'),
    ('k', 'л'),
    ('l', 'д'),
    ('z', 'я'),
    ('x', 'ч'),
    ('c', 'с'),
    ('v', 'м'),
    ('b', 'и'),
    ('n', 'т'),
    ('m', 'ь'),
    // Unshifted punctuation.
    ('[', 'х'),
    (']', 'ъ'),
    (';', 'ж'),
    ('\'', 'э'),
    (',', 'б'),
    ('.', 'ю'),
    ('/', '.'),
    ('`', 'ё'),
    // Shifted punctuation and the digit row (no case relation).
    ('{', 'Х'),
    ('}', 'Ъ'),
    (':', 'Ж'),
    ('"', 'Э'),
    ('<', 'Б'),
    ('>', 'Ю'),
    ('~', 'Ё'),
    ('@', '"'),
    ('#', '№'),
    ('$', ';'),
    ('^', ':'),
    ('&', '?'),
    ('?', ','),
    ('|', '/'),
];

fn lookup_en(byte: char) -> Option<char> {
    PAIRS.iter().find(|(en, _)| *en == byte).map(|(_, ru)| *ru)
}

fn lookup_ru(glyph: char) -> Option<char> {
    PAIRS.iter().find(|(_, ru)| *ru == glyph).map(|(en, _)| *en)
}

fn map_char(next: char, from_ru: bool) -> char {
    if from_ru {
        // Exact pairs first: shifted symbols have no case relation, so
        // case-folding their uppercase Cyrillic side would lose them
        // (',' has no uppercase; Б must map back to '<', not ',').
        if let Some(mapped) = lookup_ru(next) {
            return mapped;
        }
        // Uppercase Cyrillic derives from the lowercase pair: Й -> й -> q -> Q.
        let mut folded = next.to_lowercase();
        if let (Some(lower), None) = (folded.next(), folded.next())
            && lower != next
            && let Some(mapped) = lookup_ru(lower)
        {
            return mapped.to_ascii_uppercase();
        }
        next
    } else {
        if let Some(mapped) = lookup_en(next) {
            return mapped;
        }
        // Uppercase Latin derives the same way: Q -> q -> й -> Й.
        if next.is_ascii_uppercase() {
            return lookup_en(next.to_ascii_lowercase())
                .map(|mapped| mapped.to_uppercase().next().unwrap_or(mapped))
                .unwrap_or(next);
        }
        next
    }
}

/// Map `text` char-by-char to the other layout of the hardcoded US/RU pair.
/// `from_ru == false` converts EN->RU, `from_ru == true` RU->EN.
/// Unmapped characters (digits, emoji, third-language text, whitespace) pass
/// through unchanged.
pub fn convert(text: &str, from_ru: bool) -> String {
    text.chars().map(|next| map_char(next, from_ru)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn en_word_maps_to_ru() {
        assert_eq!(convert("ghbdtn", false), "привет");
    }

    #[test]
    fn ru_word_maps_to_en() {
        assert_eq!(convert("привет", true), "ghbdtn");
    }

    #[test]
    fn case_survives_both_directions() {
        assert_eq!(convert("Ghbdtn", false), "Привет");
        assert_eq!(convert("ПРИВЕТ", true), "GHBDTN");
    }

    #[test]
    fn punctuation_maps_both_directions() {
        assert_eq!(convert("[;'/.,`", false), "хжэ.юбё");
        assert_eq!(convert("хжэ.юбё", true), "[;'/.,`");
        assert_eq!(convert("{:~", false), "ХЖЁ");
        assert_eq!(convert("ХЖЁ", true), "{:~");
    }

    #[test]
    fn shifted_digit_row_maps() {
        assert_eq!(convert("@#$^&?", false), "\"№;:?,");
        assert_eq!(convert("\"№;:?,", true), "@#$^&?");
    }

    #[test]
    fn digits_and_identical_symbols_pass_through() {
        assert_eq!(convert("123 4%56890!-*=", false), "123 4%56890!-*=");
        assert_eq!(convert("123 4%56890!-*=", true), "123 4%56890!-*=");
    }

    #[test]
    fn emoji_and_third_language_survive() {
        assert_eq!(convert("ghbdtn 👋 你好", false), "привет 👋 你好");
        assert_eq!(convert("привет 👋 你好", true), "ghbdtn 👋 你好");
    }

    #[test]
    fn empty_stays_empty() {
        assert_eq!(convert("", false), "");
        assert_eq!(convert("", true), "");
    }

    #[test]
    fn every_pair_round_trips_both_ways() {
        for (en, ru) in PAIRS {
            assert_eq!(
                map_char(map_char(*en, false), true),
                *en,
                "en {en} did not round-trip"
            );
            assert_eq!(
                map_char(map_char(*ru, true), false),
                *ru,
                "ru {ru} did not round-trip"
            );
        }
    }

    #[test]
    fn full_alphabets_round_trip() {
        let latin: String = ('a'..='z').chain('A'..='Z').collect();
        assert_eq!(convert(&convert(&latin, false), true), latin);
        let cyrillic = "абвгдеёжзийклмнопрстуфхцчшщъыьэюяАБВГДЕЁЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯ";
        assert_eq!(convert(&convert(cyrillic, true), false), cyrillic);
    }
}
