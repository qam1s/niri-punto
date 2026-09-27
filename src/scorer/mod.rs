//! Content-based direction verdict for manual conversion.
//!
//! Renders remembered scancodes under both layouts, scores each rendering
//! with that language's bigram model ([`tables`]), and verdicts the intended
//! layout. Declines (falls back to today's `current_layout` behavior) when
//! short or below the margin.
//!
//! Tuning (wider corpus, 199 cases, zero confident-wrong): confident when
//! the average-log-prob gap reaches [`CONFIDENCE_MARGIN`] (2.6 per bigram)
//! with at least [`MIN_LETTERS`] (3) scorable characters (letters and
//! spaces). Latin input that scores strongly Us (`khorosho`, `api`,
//! `spasibo`, …) verdicts Us rather than declining: the buffer was typed
//! in the US layout, so a Us verdict is a no-op, while declining would risk
//! Cyrillic conversion after an external layout switch.
//!
//! Not wired into any conversion path yet: nothing calls [`verdict`] outside
//! tests, and the [`Detector`](crate::detector::Detector) seam keeps serving
//! `ManualOnly`. The direction wiring calls [`verdict`] (intended layout +
//! confidence); the rejected `score() -> Option<f32>` shape stays out.

mod tables;

use crate::buffer::BufferEntry;
use crate::config::LayoutPair;

/// Minimum average-log-prob gap per bigram for a confident verdict.
pub const CONFIDENCE_MARGIN: f32 = 2.6;

/// Inputs with fewer scorable characters (letters and spaces) decline.
pub const MIN_LETTERS: usize = 3;

/// Canonical scancode table: evdev scancode -> (US-side char, RU-side char)
/// for letters, space, and the punctuation covered by the selection path.
/// Digits are layout-invariant and skipped by scoring.
///
/// One source for [`render`] and both inverses below, so the arms cannot
/// drift. This is a scancode table, so it cannot reuse the char pairs in
/// [`crate::keymaps`]: those map pasted text for the selection path, while
/// scoring must start from raw scancodes before any layout applies.
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

/// Count the characters scoring can see: letters and spaces. Digits and
/// punctuation never enter the bigram sequence; spaces do (the tuning
/// counts them too, so this shape is load-bearing for the margin).
fn scorable_chars(us_text: &str) -> usize {
    us_text
        .chars()
        .filter(|c| c.is_ascii_alphabetic() || *c == ' ')
        .count()
}

/// Average log-prob per bigram over the letter/boundary sequence.
/// Returns `None` when fewer than 2 bigrams are scorable.
fn score(text: &str, symbols: &str, table: &[f32], n: usize) -> Option<f32> {
    let mut seq = vec![0usize]; // leading boundary
    for mut ch in text.chars().flat_map(|c| c.to_lowercase()) {
        if ch == ' ' {
            ch = '^';
        }
        if let Some(i) = index_of(symbols, ch) {
            seq.push(i);
        }
    }
    seq.push(0); // trailing boundary
    if seq.len() < 3 {
        return None;
    }
    let mut sum = 0.0;
    for pair in seq.windows(2) {
        sum += table[pair[0] * n + pair[1]];
    }
    Some(sum / (seq.len() - 1) as f32)
}

/// Synthesize buffer entries for selection text the daemon never typed.
///
/// Each letter maps back to the scancode whose layout side carries it
/// (Latin to the US side, Cyrillic to the RU side), so [`verdict`] scores
/// the selection exactly as if its keystrokes had been remembered. Only
/// letters and space affect scoring (the bigram symbols are letters plus
/// the boundary); anything else is skipped.
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

/// Inverse of [`render`] over the US side (letters only: the selection
/// synthesis skips every other ASCII character).
fn scancode_for_us(en: char) -> Option<u16> {
    if !en.is_ascii_alphabetic() {
        return None;
    }
    SCANCODE_TABLE
        .iter()
        .find(|(_, us, _)| *us == en)
        .map(|(scancode, _, _)| *scancode)
}

/// Inverse of [`render`] over the RU side (letters only, same as above;
/// `ё` shares `е`'s scancode).
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
    /// Pair position of the intended layout, resolved through the pair
    /// order: the Latin side holds `Us`, the Cyrillic side `Ru` (so a
    /// `layout ru,us` config verdicts the opposite positions from `us,ru`).
    pub fn index_in(self, pair: &LayoutPair) -> u8 {
        let latin = pair.latin_index();
        match self {
            Self::Us => latin,
            Self::Ru => 1 - latin,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Verdict {
    Intended(Intended),
    Decline,
}

/// Verdict the intended layout for buffer entries, or decline.
/// Confident only past [`CONFIDENCE_MARGIN`] with [`MIN_LETTERS`] scorable
/// characters; anything else declines to today's `current_layout` behavior.
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

/// Verdict two same-keystroke renderings: the US-layout text and the
/// RU-layout text for identical scancodes. [`verdict`] renders buffer
/// entries; the selection path renders clipboard text via
/// [`entries_from_text`] and calls [`verdict`] instead, so both paths share
/// one scoring core.
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

    /// Inverse for tests: us text -> buffer entries. Letters delegate to
    /// the production inverse so the arms cannot drift; punctuation and
    /// space rows stay local (scaffolding for inputs scoring skips).
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

    /// Scancodes for text typed while the RU layout was active. Same split:
    /// letters delegate, punctuation stays local.
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
    fn intended_index_follows_the_pair_order() {
        use crate::config::LayoutPair;
        let usual = LayoutPair::new("us", "ru").unwrap();
        assert_eq!(Intended::Us.index_in(&usual), 0);
        assert_eq!(Intended::Ru.index_in(&usual), 1);
        let swapped = LayoutPair::new("ru", "us").unwrap();
        assert_eq!(Intended::Us.index_in(&swapped), 1);
        assert_eq!(Intended::Ru.index_in(&swapped), 0);
    }

    #[test]
    fn ru_confident() {
        assert_verdict(
            &[
                "ghbdtn",
                "Ghj,ktvf",
                "Ghbdtn rfr ltkf", // user's sentence
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
        // Russian, but below the margin: decline is safe (fallback).
        assert_verdict(
            &[
                "aeyrwbz",      // функция
                "jib,rf",       // ошибка
                "zpsr",         // язык
                "vsim",         // мышь
                "hfcrkflrf",    // раскладка
                "gthtrk.xtybt", // переключение
                "cjj,otybt",    // сообщение
                "Xtkjdtr dbltk rybue",
                "Cgfcb,j pf ,jkmie. ghj,ktem",
                "Rfr ltkf", // Как дела
                "Dbltk rybue",
                "ynt",   // нет
                "jr",    // ок
                "lf",    // да
                "ltym",  // день
                "[kt,",  // хлеб
                "rjirf", // кошка
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
                "match", // weakest Us-confident
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
        // Latin input typed in the US layout: Us verdicts are
        // no-ops, so confidence here is safe (a decline would risk Cyrillic
        // conversion after an external layout switch). See module docs.
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
        // English-leaning but below the margin: decline is safe (fallback).
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
        // Abbreviations / tokens hostile to bigram scoring: all decline,
        // none reach confidence in either direction.
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
        // Latin input scoring near the source language: all below the
        // margin.
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
        // Whole-buffer phrase scope scores mixed content as mush: decline to
        // today's behavior. Acceptable; narrower scopes stay out of scope.
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
    fn selection_synthesis_matches_remembered_keystrokes() {
        // Latin selections verdict like the scancodes that would type them.
        for case in ["ghbdtn", "hello", "privet", "hi", ""] {
            assert_eq!(
                verdict(&entries_from_text(case)),
                verdict(&keys(case)),
                "{case}"
            );
        }
        // Cyrillic selections verdict like scancodes typed under RU.
        for case in ["привет", "мир", "окно"] {
            assert_eq!(
                verdict(&entries_from_text(case)),
                verdict(&keys_ru(case)),
                "{case}"
            );
        }
        // Unscorable characters never contribute: digits, emoji, and
        // punctuation synthesize to nothing (or to inert spaces).
        assert!(entries_from_text("123👋").is_empty());
        assert_eq!(
            verdict(&entries_from_text("ghbdtn 👋")),
            verdict(&keys("ghbdtn"))
        );
    }

    #[test]
    fn scoring_is_fast_enough_for_the_trigger_path() {
        let entries = keys(&"ghbdtn ".repeat(32)); // 224-entry buffer
        let start = std::time::Instant::now();
        for _ in 0..100 {
            let _ = verdict(&entries);
        }
        let per_call = start.elapsed() / 100;
        assert!(per_call.as_millis() < 5, "too slow: {per_call:?}");
    }
}
