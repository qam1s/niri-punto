mod tables;

use crate::buffer::BufferEntry;

pub const CONFIDENCE_MARGIN: f32 = 2.6;

pub const MIN_LETTERS: usize = 3;

const SCANCODE_TABLE: &[(u16, char, char)] = &[
    (16, 'q', 'й'),
    (17, 'w', 'ц'),
    (18, 'e', 'у'),
    (19, 'r', 'к'),
    (20, 't', 'е'),
    (21, 'y', 'н'),
    (22, 'u', 'г'),
    (23, 'i', 'ш'),
    (24, 'o', 'щ'),
    (25, 'p', 'з'),
    (26, '[', 'х'),
    (27, ']', 'ъ'),
    (30, 'a', 'ф'),
    (31, 's', 'ы'),
    (32, 'd', 'в'),
    (33, 'f', 'а'),
    (34, 'g', 'п'),
    (35, 'h', 'р'),
    (36, 'j', 'о'),
    (37, 'k', 'л'),
    (38, 'l', 'д'),
    (39, ';', 'ж'),
    (40, '\'', 'э'),
    (41, '`', 'ё'),
    (44, 'z', 'я'),
    (45, 'x', 'ч'),
    (46, 'c', 'с'),
    (47, 'v', 'м'),
    (48, 'b', 'и'),
    (49, 'n', 'т'),
    (50, 'm', 'ь'),
    (51, ',', 'б'),
    (52, '.', 'ю'),
    (53, '/', '.'),
    (57, ' ', ' '),
];

fn render(entry: BufferEntry) -> Option<(char, char)> {
    let (_, en, ru) = SCANCODE_TABLE
        .iter()
        .find(|(scancode, _, _)| *scancode == entry.scancode)?;
    if entry.shift && en.is_ascii_alphabetic() {
        Some((en.to_ascii_uppercase(), *ru))
    } else {
        Some((*en, *ru))
    }
}

fn index_of(symbols: &str, ch: char) -> Option<usize> {
    symbols.chars().position(|c| c == ch)
}

fn scorable_chars(us_text: &str) -> usize {
    us_text
        .chars()
        .filter(|c| c.is_ascii_alphabetic() || *c == ' ')
        .count()
}

fn score(text: &str, symbols: &str, table: &[f32], n: usize) -> Option<f32> {
    let mut seq = vec![0usize];
    for mut ch in text.chars().flat_map(|c| c.to_lowercase()) {
        if ch == ' ' {
            ch = '^';
        }
        if let Some(i) = index_of(symbols, ch) {
            seq.push(i);
        }
    }
    seq.push(0);
    if seq.len() < 3 {
        return None;
    }
    let mut sum = 0.0;
    for pair in seq.windows(2) {
        sum += table[pair[0] * n + pair[1]];
    }
    Some(sum / (seq.len() - 1) as f32)
}

#[cfg(test)]
pub fn entries_from_text(text: &str) -> Vec<BufferEntry> {
    let mut out = Vec::new();
    for ch in text.chars() {
        if ch == ' ' {
            out.push(BufferEntry {
                scancode: 57,
                shift: false,
            });
        } else if ch.is_ascii_alphabetic() {
            if let Some(scancode) = scancode_for_us(ch.to_ascii_lowercase()) {
                out.push(BufferEntry {
                    scancode,
                    shift: ch.is_ascii_uppercase(),
                });
            }
        } else if !ch.is_ascii() {
            let lower = ch.to_lowercase().next().unwrap_or(ch);
            if let Some(scancode) = scancode_for_ru(lower) {
                out.push(BufferEntry {
                    scancode,
                    shift: false,
                });
            }
        }
    }
    out
}

#[cfg(test)]
fn scancode_for_us(en: char) -> Option<u16> {
    if !en.is_ascii_alphabetic() {
        return None;
    }
    SCANCODE_TABLE
        .iter()
        .find(|(_, us, _)| *us == en)
        .map(|(scancode, _, _)| *scancode)
}

#[cfg(test)]
fn scancode_for_ru(ru: char) -> Option<u16> {
    if ru == 'ё' {
        return Some(20);
    }
    SCANCODE_TABLE
        .iter()
        .find(|(_, us, cyrillic)| *cyrillic == ru && us.is_ascii_alphabetic())
        .map(|(scancode, _, _)| *scancode)
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Intended {
    Us,
    Ru,
}

impl Intended {
    pub fn index_in(self, latin: u8) -> u8 {
        match self {
            Self::Us => latin,
            Self::Ru => 1 - latin,
        }
    }
}

/// Which niri layout index renders latin script: the first name that is
/// not a known cyrillic layout, or 0 when none matches.
pub fn latin_index(names: &[String]) -> u8 {
    names
        .iter()
        .position(|name| !is_cyrillic(name))
        .unwrap_or(0) as u8
}

fn is_cyrillic(name: &str) -> bool {
    name.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| {
            matches!(
                word,
                "ru" | "ua"
                    | "by"
                    | "bg"
                    | "mk"
                    | "rs"
                    | "kz"
                    | "kg"
                    | "tj"
                    | "mn"
                    | "russian"
                    | "ukrainian"
                    | "belarusian"
                    | "bulgarian"
                    | "macedonian"
                    | "serbian"
                    | "kazakh"
                    | "kyrgyz"
                    | "tajik"
                    | "mongolian"
            )
        })
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Verdict {
    Intended(Intended),
    Decline,
}

pub fn verdict(entries: &[BufferEntry]) -> Verdict {
    let mut us_text = String::new();
    let mut ru_text = String::new();
    for &entry in entries {
        if let Some((en, ru)) = render(entry) {
            us_text.push(en);
            ru_text.push(ru);
        }
    }
    verdict_rendered(&us_text, &ru_text)
}

fn verdict_rendered(us_text: &str, ru_text: &str) -> Verdict {
    if scorable_chars(us_text) < MIN_LETTERS {
        return Verdict::Decline;
    }
    let us = score(us_text, tables::EN_SYMBOLS, &tables::EN, tables::EN_N);
    let ru = score(ru_text, tables::RU_SYMBOLS, &tables::RU, tables::RU_N);
    match (us, ru) {
        (Some(u), Some(r)) => {
            if u - r >= CONFIDENCE_MARGIN {
                Verdict::Intended(Intended::Us)
            } else if r - u >= CONFIDENCE_MARGIN {
                Verdict::Intended(Intended::Ru)
            } else {
                Verdict::Decline
            }
        }
        _ => Verdict::Decline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(us_text: &str) -> Vec<BufferEntry> {
        let mut out = Vec::new();
        for ch in us_text.chars() {
            let (lower, shift) = if ch.is_ascii_uppercase() {
                (ch.to_ascii_lowercase(), true)
            } else {
                (ch, false)
            };
            let scancode = match lower {
                '[' => 26,
                ']' => 27,
                ';' => 39,
                '\'' => 40,
                '`' => 41,
                ',' => 51,
                '.' => 52,
                '/' => 53,
                ' ' => 57,
                _ => match scancode_for_us(lower) {
                    Some(scancode) => scancode,
                    None => continue,
                },
            };
            out.push(BufferEntry { scancode, shift });
        }
        out
    }

    fn keys_ru(ru_text: &str) -> Vec<BufferEntry> {
        let mut out = Vec::new();
        for ch in ru_text.chars().flat_map(|c| c.to_lowercase()) {
            let scancode = match ch {
                'х' => 26,
                'ъ' => 27,
                'ж' => 39,
                'э' => 40,
                'б' => 51,
                'ю' => 52,
                '.' => 53,
                ' ' => 57,
                _ => match scancode_for_ru(ch) {
                    Some(scancode) => scancode,
                    None => continue,
                },
            };
            out.push(BufferEntry {
                scancode,
                shift: false,
            });
        }
        out
    }

    fn assert_verdict(cases: &[&str], expected: Verdict) {
        for case in cases {
            assert_eq!(verdict(&keys(case)), expected, "{case}");
        }
    }

    #[test]
    fn tuning_constants_match_the_wider_corpus() {
        assert_eq!(CONFIDENCE_MARGIN, 2.6);
        assert_eq!(MIN_LETTERS, 3);
    }

    #[test]
    fn intended_index_follows_the_latin_position() {
        assert_eq!(Intended::Us.index_in(0), 0);
        assert_eq!(Intended::Ru.index_in(0), 1);
        assert_eq!(Intended::Us.index_in(1), 1);
        assert_eq!(Intended::Ru.index_in(1), 0);
    }

    #[test]
    fn latin_index_names_the_non_cyrillic_position() {
        fn names(list: &[&str]) -> Vec<String> {
            list.iter().map(|name| name.to_string()).collect()
        }
        assert_eq!(latin_index(&names(&["us", "ru"])), 0);
        assert_eq!(latin_index(&names(&["ru", "us"])), 1);
        assert_eq!(latin_index(&names(&["English (US)", "Russian"])), 0);
        assert_eq!(latin_index(&names(&["Russian", "English (US)"])), 1);
        assert_eq!(latin_index(&names(&["ru+phonetic", "us"])), 1);
        assert_eq!(latin_index(&names(&["de", "fr"])), 0);
        assert_eq!(latin_index(&names(&["ru", "ua"])), 0);
        assert_eq!(latin_index(&[]), 0);
    }

    #[test]
    fn ru_confident() {
        assert_verdict(
            &[
                "ghbdtn",
                "Ghj,ktvf",
                "Ghbdtn rfr ltkf",
                "Ghjuhfvvbhjdfybz",
                "cnfdrb",
                "cgfcb,j",
                "gj;fkeqcnf",
                "plhfdcmdeqnt",
                "lj cdblybz",
                "[jhjij",
                "ghj,ktvf",
                "vtyz pjden",
                "gthtvtyyfz",
                "rjvgm.nth",
                "rkfdbfnehf",
                "xtkjdtr",
                "dhtvz",
                "Rjvgm.nth hf,jnftn",
                "Vjz hf,jnf",
                "Lj,hjt enjh",
                "jryj",
                "yjxm",
                "enjh",
                "ljv",
                "djlf",
                "vjkjrj",
                "cj,frf",
            ],
            Verdict::Intended(Intended::Ru),
        );
    }

    #[test]
    fn ru_weak_declines() {
        assert_verdict(
            &[
                "aeyrwbz",
                "jib,rf",
                "zpsr",
                "vsim",
                "hfcrkflrf",
                "gthtrk.xtybt",
                "cjj,otybt",
                "Xtkjdtr dbltk rybue",
                "Cgfcb,j pf ,jkmie. ghj,ktem",
                "Rfr ltkf",
                "Dbltk rybue",
                "ynt",
                "jr",
                "lf",
                "ltym",
                "[kt,",
                "rjirf",
            ],
            Verdict::Decline,
        );
    }

    #[test]
    fn us_confident() {
        assert_verdict(
            &[
                "hello",
                "keyboard",
                "switch layout",
                "sudo apt update",
                "this is a test",
                "the quick brown fox",
                "a journey through the night",
                "cargo test failed",
                "merge conflict in main",
                "fix the failing build",
                "restart the window manager",
                "type to search settings",
                "battery level is low",
                "connected to wifi",
                "screenshot saved to pictures",
                "copy paste with clipboard",
                "the terminal froze again",
                "reboot the machine now",
                "volume up and down",
                "window",
                "layout",
                "cursor",
                "frame",
                "focus",
                "workspace",
                "monitor",
                "scroll",
                "toggle",
                "convert",
                "score",
                "margin",
                "space",
                "language",
                "russian",
                "english",
                "config",
                "commit",
                "import",
                "export",
                "match",
                "value",
                "output",
                "for",
                "you",
                "this",
                "from",
                "have",
                "docker",
                "token",
                "proxy",
                "var",
                "mut",
                "main",
                "root",
                "niri",
                "wayland",
                "google",
                "localhost",
                "home/user/documents",
                "/etc/niri/config.kdl",
                "http://localhost:8080/api/v1/users",
                "ssh",
                "api",
                "json",
                "css",
            ],
            Verdict::Intended(Intended::Us),
        );
    }

    #[test]
    fn latin_input_us_confident() {
        assert_verdict(
            &[
                "spasibo",
                "pozhaluysta",
                "dosvidaniya",
                "khorosho",
                "poka",
                "menya zovut",
                "do svidaniya",
                "bolshoe spasibo",
                "dobroe utro",
                "spokoynoy nochi",
            ],
            Verdict::Intended(Intended::Us),
        );
    }

    #[test]
    fn us_weak_declines() {
        assert_verdict(
            &[
                "open a pull request",
                "buffer",
                "input",
                "render",
                "detect",
                "entry",
                "shift",
                "letter",
                "sentence",
                "keymap",
                "struct",
                "default",
                "return",
                "index",
                "str",
                "cache",
                "query",
                "python",
                "linux",
            ],
            Verdict::Decline,
        );
    }

    #[test]
    fn us_short_declines() {
        assert_verdict(
            &[
                "hi", "me", "we", "up", "on", "is", "it", "to", "do", "go", "an", "as", "at", "be",
                "by", "or", "if", "of", "in", "us", "the", "and", "with", "that", "net", "ok",
                "ff",
            ],
            Verdict::Decline,
        );
    }

    #[test]
    fn hostile_tech_declines() {
        assert_verdict(
            &[
                "http", "https", "dns", "url", "yaml", "html", "src", "lib", "bin", "etc", "cfg",
                "tmp", "fn", "let", "ref", "vec", "impl", "github",
            ],
            Verdict::Decline,
        );
    }

    #[test]
    fn urls_paths_and_degenerate_decline() {
        assert_verdict(
            &[
                "https://github.com/user/repo",
                "src/main.rs",
                "",
                "123",
                ",./",
            ],
            Verdict::Decline,
        );
    }

    #[test]
    fn ambiguous_latin_declines() {
        assert_verdict(
            &[
                "privet",
                "privet kak dela",
                "kak dela",
                "zdravstvuyte",
                "ya lyublyu tebya",
            ],
            Verdict::Decline,
        );
    }

    #[test]
    fn mixed_language_buffers_decline() {
        for (us, ru) in [
            ("hello ", "мир"),
            ("switch ", "окно"),
            ("test ", "привет"),
            ("layout ", "раскладка"),
            ("good morning ", "доброе утро"),
            ("my work ", "моя работа"),
        ] {
            let mut entries = keys(us);
            entries.extend(keys_ru(ru));
            assert_eq!(verdict(&entries), Verdict::Decline, "mixed {us:?} + {ru:?}");
        }
    }

    #[test]
    fn text_synthesis_matches_remembered_keystrokes() {
        for case in ["ghbdtn", "hello", "privet", "hi", ""] {
            assert_eq!(
                verdict(&entries_from_text(case)),
                verdict(&keys(case)),
                "{case}"
            );
        }
        for case in ["привет", "мир", "окно"] {
            assert_eq!(
                verdict(&entries_from_text(case)),
                verdict(&keys_ru(case)),
                "{case}"
            );
        }
        assert!(entries_from_text("123👋").is_empty());
        assert_eq!(
            verdict(&entries_from_text("ghbdtn 👋")),
            verdict(&keys("ghbdtn"))
        );
    }

    #[test]
    fn scoring_is_fast_enough_for_the_trigger_path() {
        let entries = keys(&"ghbdtn ".repeat(32));
        let start = std::time::Instant::now();
        for _ in 0..100 {
            let _ = verdict(&entries);
        }
        let per_call = start.elapsed() / 100;
        assert!(per_call.as_millis() < 5, "too slow: {per_call:?}");
    }
}
