//! Symbol tables for selection conversion, generated at build time.

use std::sync::OnceLock;

/// The keymap baked by `build.rs`.
const BAKED_KEYMAP: &str = include_str!(concat!(env!("OUT_DIR"), "/xkb_keymap.xkb"));

/// One bidirectional character pair: US QWERTY position <-> RU JCUKEN glyph.
const PAIRS: &[(char, char)] = &[
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
    ('[', 'х'),
    (']', 'ъ'),
    (';', 'ж'),
    ('\'', 'э'),
    (',', 'б'),
    ('.', 'ю'),
    ('/', '.'),
    ('`', 'ё'),
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

fn lookup_en(pairs: &[(char, char)], byte: char) -> Option<char> {
    pairs.iter().find(|(en, _)| *en == byte).map(|(_, ru)| *ru)
}

fn lookup_ru(pairs: &[(char, char)], glyph: char) -> Option<char> {
    pairs.iter().find(|(_, ru)| *ru == glyph).map(|(en, _)| *en)
}

fn map_char_in(pairs: &[(char, char)], ch: char, from_ru: bool) -> char {
    if from_ru {
        if let Some(mapped) = lookup_ru(pairs, ch) {
            return mapped;
        }
        let mut folded = ch.to_lowercase();
        if let (Some(lower), None) = (folded.next(), folded.next())
            && lower != ch
            && let Some(mapped) = lookup_ru(pairs, lower)
        {
            return mapped.to_ascii_uppercase();
        }
        ch
    } else {
        if let Some(mapped) = lookup_en(pairs, ch) {
            return mapped;
        }
        if ch.is_ascii_uppercase() {
            return lookup_en(pairs, ch.to_ascii_lowercase())
                .map(|mapped| mapped.to_uppercase().next().unwrap_or(mapped))
                .unwrap_or(ch);
        }
        ch
    }
}

/// Map `text` char-by-char to the other layout of the active table.
pub fn convert(text: &str, from_ru: bool) -> String {
    convert_in(active_pairs(), text, from_ru)
}

fn convert_in(pairs: &[(char, char)], text: &str, from_ru: bool) -> String {
    text.chars()
        .map(|ch| map_char_in(pairs, ch, from_ru))
        .collect()
}

fn active_pairs() -> &'static [(char, char)] {
    static ACTIVE: OnceLock<Vec<(char, char)>> = OnceLock::new();
    ACTIVE.get_or_init(|| resolve_pairs(parse_xkbcli_keymap(BAKED_KEYMAP)))
}

fn resolve_pairs(generated: Vec<(char, char)>) -> Vec<(char, char)> {
    if generated.is_empty() {
        PAIRS.to_vec()
    } else {
        generated
    }
}

/// Parse an `xkbcli compile-keymap` dump into (latin, cyrillic) pairs.
pub fn parse_xkbcli_keymap(text: &str) -> Vec<(char, char)> {
    let mut pairs = Vec::new();
    let mut first: Option<Vec<String>> = None;
    let mut second: Option<Vec<String>> = None;
    let mut in_key = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("key <") {
            in_key = true;
            first = None;
            second = None;
        } else if in_key && trimmed.starts_with("symbols[1]=") {
            first = Some(symbols_in(trimmed));
        } else if in_key && trimmed.starts_with("symbols[2]=") {
            second = Some(symbols_in(trimmed));
        } else if in_key && trimmed == "};" {
            in_key = false;
            if let (Some(group_en), Some(group_ru)) = (first.take(), second.take()) {
                for level in 0..2 {
                    let sides = (
                        group_en.get(level).and_then(|name| keysym_to_char(name)),
                        group_ru.get(level).and_then(|name| keysym_to_char(name)),
                    );
                    let (Some(en), Some(ru)) = sides else {
                        continue;
                    };
                    if en != ru && !pairs.contains(&(en, ru)) {
                        pairs.push((en, ru));
                    }
                }
            }
        }
    }
    pairs
}

/// The symbol list of one `symbols[N]= [...]` line.
fn symbols_in(line: &str) -> Vec<String> {
    let after_eq = line.split('=').nth(1).unwrap_or("");
    let (Some(start), Some(end)) = (after_eq.find('['), after_eq.rfind(']')) else {
        return Vec::new();
    };
    after_eq[start + 1..end]
        .split(',')
        .map(|symbol| symbol.trim().to_string())
        .collect()
}

fn keysym_to_char(name: &str) -> Option<char> {
    if let Some(rest) = name.strip_prefix("Cyrillic_") {
        let lower = rest.to_lowercase();
        let base = CYRILLIC
            .iter()
            .find(|(keysym, _)| *keysym == lower)
            .map(|(_, ch)| *ch)?;
        if rest.chars().all(|c| c.is_lowercase()) {
            Some(base)
        } else {
            base.to_uppercase().next()
        }
    } else if let Some(plain) = NAMED.iter().find(|(keysym, _)| *keysym == name) {
        Some(plain.1)
    } else if name.len() == 1 {
        name.chars().next()
    } else if let Some(hex) = name
        .strip_prefix('U')
        .filter(|hex| !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()))
    {
        u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
    } else {
        None
    }
}

/// Named non-Cyrillic keysyms appearing in the pair's keymaps.
const NAMED: &[(&str, char)] = &[
    ("bracketleft", '['),
    ("bracketright", ']'),
    ("braceleft", '{'),
    ("braceright", '}'),
    ("semicolon", ';'),
    ("colon", ':'),
    ("apostrophe", '\''),
    ("quotedbl", '"'),
    ("comma", ','),
    ("less", '<'),
    ("period", '.'),
    ("greater", '>'),
    ("slash", '/'),
    ("question", '?'),
    ("grave", '`'),
    ("asciitilde", '~'),
    ("minus", '-'),
    ("underscore", '_'),
    ("equal", '='),
    ("plus", '+'),
    ("exclam", '!'),
    ("at", '@'),
    ("numbersign", '#'),
    ("dollar", '$'),
    ("percent", '%'),
    ("asciicircum", '^'),
    ("ampersand", '&'),
    ("asterisk", '*'),
    ("parenleft", '('),
    ("parenright", ')'),
    ("bar", '|'),
    ("backslash", '\\'),
    ("numerosign", '№'),
];

/// Lowercase `Cyrillic_*` keysym remainders to characters.
const CYRILLIC: &[(&str, char)] = &[
    ("io", 'ё'),
    ("shorti", 'й'),
    ("tse", 'ц'),
    ("u", 'у'),
    ("ka", 'к'),
    ("ie", 'е'),
    ("en", 'н'),
    ("ghe", 'г'),
    ("sha", 'ш'),
    ("shcha", 'щ'),
    ("ze", 'з'),
    ("ha", 'х'),
    ("hardsign", 'ъ'),
    ("ef", 'ф'),
    ("yeru", 'ы'),
    ("ve", 'в'),
    ("a", 'а'),
    ("pe", 'п'),
    ("er", 'р'),
    ("o", 'о'),
    ("el", 'л'),
    ("de", 'д'),
    ("zhe", 'ж'),
    ("e", 'э'),
    ("ya", 'я'),
    ("che", 'ч'),
    ("es", 'с'),
    ("em", 'м'),
    ("i", 'и'),
    ("te", 'т'),
    ("softsign", 'ь'),
    ("be", 'б'),
    ("yu", 'ю'),
];

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
                map_char_in(PAIRS, map_char_in(PAIRS, *en, false), true),
                *en,
                "en {en} did not round-trip"
            );
            assert_eq!(
                map_char_in(PAIRS, map_char_in(PAIRS, *ru, true), false),
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

    /// Canned `xkbcli compile-keymap` fragment.
    const SAMPLE: &str = "\
\tkey <AD01>               {
\t\tsymbols[1]= [               q,               Q ],
\t\tsymbols[2]= [ Cyrillic_shorti, Cyrillic_SHORTI ]
\t};
\tkey <AE04>               {
\t\tsymbols[1]= [               4,          dollar ],
\t\tsymbols[2]= [               4,       semicolon ]
\t};
\tkey <AE09>               {
\t\tsymbols[1]= [               9,       parenleft ],
\t\tsymbols[2]= [               9,       parenleft ]
\t};
\tkey <LFSH>               {
\t\tsymbols[1]= [         Shift_L,  ISO_Next_Group ],
\t\tsymbols[2]= [       backslash,           slash ]
\t};
";

    #[test]
    fn xkbcli_sample_parses_to_letter_and_shifted_pairs() {
        assert_eq!(
            parse_xkbcli_keymap(SAMPLE),
            vec![('q', 'й'), ('Q', 'Й'), ('$', ';')]
        );
    }

    #[test]
    fn xkbcli_identical_and_modifier_sides_are_skipped() {
        let pairs = parse_xkbcli_keymap(SAMPLE);
        assert!(!pairs.iter().any(|(en, _)| *en == '9'));
        assert!(!pairs.iter().any(|(en, _)| *en == '\\'));
    }

    #[test]
    fn xkbcli_canned_table_converts_both_ways() {
        let pairs = parse_xkbcli_keymap(SAMPLE);
        assert_eq!(convert_in(&pairs, "q$", false), "й;");
        assert_eq!(convert_in(&pairs, "й;", true), "q$");
    }

    #[test]
    fn xkbcli_uppercase_cyrillic_derives_from_lowercase() {
        assert_eq!(keysym_to_char("Cyrillic_SHORTI"), Some('Й'));
        assert_eq!(keysym_to_char("Cyrillic_shorti"), Some('й'));
        assert_eq!(keysym_to_char("Cyrillic_HARDSIGN"), Some('Ъ'));
    }

    #[test]
    fn xkbcli_unresolvable_names_are_none() {
        assert_eq!(keysym_to_char("Shift_L"), None);
        assert_eq!(keysym_to_char("ISO_Next_Group"), None);
        assert_eq!(keysym_to_char("NoSymbol"), None);
        assert_eq!(keysym_to_char("XF86TouchpadToggle"), None);
    }

    #[test]
    fn xkbcli_garbage_parses_to_empty_for_fallback() {
        assert!(parse_xkbcli_keymap("").is_empty());
        assert!(parse_xkbcli_keymap("not a keymap\n").is_empty());
    }

    #[test]
    fn empty_generated_table_selects_the_static_fallback() {
        assert_eq!(resolve_pairs(Vec::new()), PAIRS.to_vec());
        let generated = vec![('q', 'й')];
        assert_eq!(resolve_pairs(generated.clone()), generated);
    }
}
