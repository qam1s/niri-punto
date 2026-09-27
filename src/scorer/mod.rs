//! Content-based direction verdict for manual conversion.
//!
//! Renders remembered scancodes under both layouts, scores each rendering
//! with that language's bigram model ([`tables`]), and verdicts the intended
//! layout. Declines (falls back to today's `current_layout` behavior) when
//! short or below the margin.
//!
//! Tuning (wider corpus, 199 cases, zero confident-wrong): confident when
//! the average-log-prob gap reaches [`CONFIDENCE_MARGIN`] (2.6 per bigram)
//! with at least [`MIN_LETTERS`] (3) scorable letters. Transliteration and
//! tech Latin that scores strongly Us (`khorosho`, `api`, `spasibo`, …)
//! verdicts Us rather than declining: the buffer was typed in the US
//! layout, so a Us verdict is a no-op, while declining would risk Cyrillic
//! conversion after an external layout switch.
//!
//! Not wired into any conversion path yet: nothing calls [`verdict`] outside
//! tests, and the [`Detector`](crate::detector::Detector) seam keeps serving
//! `ManualOnly`. The direction wiring calls [`verdict`] (intended layout +
//! confidence); the rejected `score() -> Option<f32>` shape stays out.

mod tables;

use crate::buffer::BufferEntry;

/// Minimum average-log-prob gap per bigram for a confident verdict.
pub const CONFIDENCE_MARGIN: f32 = 2.6;

/// Inputs with fewer scorable letters always decline.
pub const MIN_LETTERS: usize = 3;

/// evdev scancode -> (us char, ru char) for letters, space, and the
/// punctuation covered by the selection path. Digits are layout-invariant
/// and skipped by scoring.
///
/// This is a scancode table, so it cannot reuse the char pairs in
/// [`crate::keymaps`]: those map pasted text for the selection path, while
/// scoring must start from raw scancodes before any layout applies.
fn render(entry: BufferEntry) -> Option<(char, char)> {
    let (en, ru) = match entry.scancode {
        16 => ('q', 'й'),
        17 => ('w', 'ц'),
        18 => ('e', 'у'),
        19 => ('r', 'к'),
        20 => ('t', 'е'),
        21 => ('y', 'н'),
        22 => ('u', 'г'),
        23 => ('i', 'ш'),
        24 => ('o', 'щ'),
        25 => ('p', 'з'),
        30 => ('a', 'ф'),
        31 => ('s', 'ы'),
        32 => ('d', 'в'),
        33 => ('f', 'а'),
        34 => ('g', 'п'),
        35 => ('h', 'р'),
        36 => ('j', 'о'),
        37 => ('k', 'л'),
        38 => ('l', 'д'),
        44 => ('z', 'я'),
        45 => ('x', 'ч'),
        46 => ('c', 'с'),
        47 => ('v', 'м'),
        48 => ('b', 'и'),
        49 => ('n', 'т'),
        50 => ('m', 'ь'),
        26 => ('[', 'х'),
        27 => (']', 'ъ'),
        39 => (';', 'ж'),
        40 => ('\'', 'э'),
        41 => ('`', 'ё'),
        51 => (',', 'б'),
        52 => ('.', 'ю'),
        53 => ('/', '.'),
        57 => (' ', ' '),
        _ => return None,
    };
    if entry.shift && en.is_ascii_alphabetic() {
        Some((en.to_ascii_uppercase(), ru))
    } else {
        Some((en, ru))
    }
}

fn index_of(symbols: &str, ch: char) -> Option<usize> {
    symbols.chars().position(|c| c == ch)
}

fn scorable_letters(us_text: &str) -> usize {
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

/// Inverse of [`render`] over the US side.
fn scancode_for_us(en: char) -> Option<u16> {
    let scancode = match en {
        'q' => 16,
        'w' => 17,
        'e' => 18,
        'r' => 19,
        't' => 20,
        'y' => 21,
        'u' => 22,
        'i' => 23,
        'o' => 24,
        'p' => 25,
        'a' => 30,
        's' => 31,
        'd' => 32,
        'f' => 33,
        'g' => 34,
        'h' => 35,
        'j' => 36,
        'k' => 37,
        'l' => 38,
        'z' => 44,
        'x' => 45,
        'c' => 46,
        'v' => 47,
        'b' => 48,
        'n' => 49,
        'm' => 50,
        _ => return None,
    };
    Some(scancode)
}

/// Inverse of [`render`] over the RU side.
fn scancode_for_ru(ru: char) -> Option<u16> {
    let scancode = match ru {
        'й' => 16,
        'ц' => 17,
        'у' => 18,
        'к' => 19,
        'е' | 'ё' => 20,
        'н' => 21,
        'г' => 22,
        'ш' => 23,
        'щ' => 24,
        'з' => 25,
        'ф' => 30,
        'ы' => 31,
        'в' => 32,
        'а' => 33,
        'п' => 34,
        'р' => 35,
        'о' => 36,
        'л' => 37,
        'д' => 38,
        'я' => 44,
        'ч' => 45,
        'с' => 46,
        'м' => 47,
        'и' => 48,
        'т' => 49,
        'ь' => 50,
        _ => return None,
    };
    Some(scancode)
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Intended {
    Us,
    Ru,
}

impl Intended {
    /// Pair position of the intended layout: the first pair entry is the
    /// Latin side, the second the Cyrillic side (see the `layouts` config).
    pub fn index(self) -> u8 {
        match self {
            Self::Us => 0,
            Self::Ru => 1,
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
/// letters; anything else declines to today's `current_layout` behavior.
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
    if scorable_letters(us_text) < MIN_LETTERS {
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

    /// Inverse for tests: us text -> buffer entries.
    fn keys(us_text: &str) -> Vec<BufferEntry> {
        let mut out = Vec::new();
        for ch in us_text.chars() {
            let (lower, shift) = if ch.is_ascii_uppercase() {
                (ch.to_ascii_lowercase(), true)
            } else {
                (ch, false)
            };
            let scancode = match lower {
                'q' => 16,
                'w' => 17,
                'e' => 18,
                'r' => 19,
                't' => 20,
                'y' => 21,
                'u' => 22,
                'i' => 23,
                'o' => 24,
                'p' => 25,
                'a' => 30,
                's' => 31,
                'd' => 32,
                'f' => 33,
                'g' => 34,
                'h' => 35,
                'j' => 36,
                'k' => 37,
                'l' => 38,
                'z' => 44,
                'x' => 45,
                'c' => 46,
                'v' => 47,
                'b' => 48,
                'n' => 49,
                'm' => 50,
                '[' => 26,
                ']' => 27,
                ';' => 39,
                '\'' => 40,
                '`' => 41,
                ',' => 51,
                '.' => 52,
                '/' => 53,
                ' ' => 57,
                _ => continue,
            };
            out.push(BufferEntry { scancode, shift });
        }
        out
    }

    /// Scancodes for text typed while the RU layout was active.
    fn keys_ru(ru_text: &str) -> Vec<BufferEntry> {
        let mut out = Vec::new();
        for ch in ru_text.chars().flat_map(|c| c.to_lowercase()) {
            let scancode = match ch {
                'й' => 16,
                'ц' => 17,
                'у' => 18,
                'к' => 19,
                'е' | 'ё' => 20,
                'н' => 21,
                'г' => 22,
                'ш' => 23,
                'щ' => 24,
                'з' => 25,
                'ф' => 30,
                'ы' => 31,
                'в' => 32,
                'а' => 33,
                'п' => 34,
                'р' => 35,
                'о' => 36,
                'л' => 37,
                'д' => 38,
                'я' => 44,
                'ч' => 45,
                'с' => 46,
                'м' => 47,
                'и' => 48,
                'т' => 49,
                'ь' => 50,
                'х' => 26,
                'ъ' => 27,
                'ж' => 39,
                'э' => 40,
                'б' => 51,
                'ю' => 52,
                '.' => 53,
                ' ' => 57,
                _ => continue,
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
    fn transliteration_us_confident() {
        // Latin transliteration typed in the US layout: Us verdicts are
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
    fn transliteration_ambiguous_declines() {
        // Transliteration indistinguishable from source-language text: all
        // below the margin.
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
        // today's behavior. Acceptable, not a fix.
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
