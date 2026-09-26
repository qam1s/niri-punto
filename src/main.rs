//! niri-punto: Punto Switcher-style daemon for the niri compositor.
//!
//! Ticket 07 (dry-run reader): read real key presses, recognize gestures,
//! log them. No injection, no layout switching.

mod buffer;
mod reader;
mod trigger;

use buffer::InputBuffer;
use reader::Reader;
use std::path::PathBuf;
use std::time::Instant;
use trigger::{GestureKind, TriggerMachine};

fn usage() -> ! {
    eprintln!("usage: niri-punto [--input-dir DIR]");
    eprintln!("  Reads keys without grabbing, logs recognized words and");
    eprintln!("  gestures. Never injects input or switches layouts.");
    std::process::exit(2);
}

fn main() {
    let mut input_dir = PathBuf::from("/dev/input");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input-dir" => {
                input_dir = args.next().map(PathBuf::from).unwrap_or_else(|| usage());
            }
            "-h" | "--help" => usage(),
            _ => usage(),
        }
    }

    eprintln!("niri-punto dry-run: logging words and gestures, changing nothing");
    eprintln!("input dir: {}", input_dir.display());
    eprintln!(
        "double-shift window: {}ms, undo window: {}ms",
        trigger::DOUBLE_SHIFT_WINDOW_MS,
        trigger::UNDO_WINDOW_MS
    );

    let mut reader = Reader::open(&input_dir);
    if reader.live_devices().is_empty() {
        eprintln!("warning: no keyboards open; waiting for hotplug");
    }

    let start = Instant::now();
    let mut triggers = TriggerMachine::new();
    let mut buffer = InputBuffer::default();

    loop {
        let Some(raw) = reader.next_key(&input_dir) else {
            eprintln!("input: no devices left, exiting");
            std::process::exit(1);
        };
        let now_ms = start.elapsed().as_millis() as u64;
        let key = reader::classify(raw.scancode);

        if reader::is_typing_key(key, raw.value) {
            if raw.value == 1 {
                if reader::is_reset(raw.scancode) {
                    buffer.clear();
                    eprintln!("buffer: cleared (esc)");
                } else {
                    buffer.push(reader::buffer_entry_for(
                        raw.scancode,
                        triggers.shift_held(),
                    ));
                    if reader::is_word_boundary(raw.scancode) {
                        // The boundary itself is excluded from the word, so
                        // this counts the finished word only.
                        let word_len = buffer.trailing_word(reader::is_word_boundary).len();
                        eprintln!("word: {word_len} entries (history: {})", buffer.len());
                    }
                }
            } else {
                // Autorepeat: the character repeats on screen, so it repeats
                // in the buffer too.
                buffer.push(reader::buffer_entry_for(
                    raw.scancode,
                    triggers.shift_held(),
                ));
            }
        }

        let pressed = raw.value != 0;
        if let Some(gesture) = triggers.key(key, pressed, now_ms) {
            let (scope, codes): (&str, Vec<u16>) = match gesture.kind {
                GestureKind::Word => (
                    "word",
                    buffer
                        .trailing_word(reader::is_word_boundary)
                        .iter()
                        .map(|e| e.scancode)
                        .collect(),
                ),
                GestureKind::Phrase => (
                    "phrase",
                    buffer.entries().iter().map(|e| e.scancode).collect(),
                ),
                GestureKind::Selection => ("selection", Vec::new()),
            };
            if gesture.undo {
                eprintln!("gesture: {scope} (undo of previous conversion)");
            } else {
                eprintln!("gesture: {scope} (scancodes: {codes:?})");
            }
        }
    }
}
