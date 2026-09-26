//! niri-punto: Punto Switcher-style daemon for the niri compositor.
//!
//! Ticket 08 (word conversion): read real key presses, and on Double Shift
//! erase the last word via uinput, switch the layout by explicit index over
//! niri IPC, wait for the layout-changed event, then replay the same
//! scancodes. Repeating the gesture undoes the conversion.
//! Ticket 09 (selection): on Ctrl+DoubleShift read the primary selection via
//! `wl-paste`, map it char-by-char to the other layout, publish it with
//! `wl-copy`, switch the layout by index, wait for the layout-changed event,
//! then paste over the selection with Ctrl+V. Repeating the gesture undoes.
//! The phrase gesture is recognized but wired by ticket 10.

mod buffer;
mod clipboard;
mod convert;
mod inject;
mod ipc;
mod keymaps;
mod reader;
mod selection;
mod trigger;

use buffer::InputBuffer;
use convert::{ConversionPlan, Converter};
use inject::Injector;
use ipc::IpcClient;
use reader::Reader;
use selection::{SelectionConverter, SelectionPlan};
use std::path::PathBuf;
use std::time::Instant;
use trigger::{GestureKind, TriggerMachine};

fn usage() -> ! {
    eprintln!("usage: niri-punto [--input-dir DIR]");
    eprintln!("  Converts the last word on Double Shift; repeating the");
    eprintln!("  gesture undoes. Needs /dev/uinput and $NIRI_SOCKET.");
    std::process::exit(2);
}

/// Erase, switch by index, wait for the layout-changed event, then replay.
/// The buffer stays frozen (see [`convert`]); only the [`Converter`] state
/// advances, which the caller already updated.
fn apply_plan(ipc: &mut IpcClient, injector: &mut Injector, plan: &ConversionPlan) {
    if let Err(error) = injector.erase(plan.erase) {
        eprintln!("convert: erase failed: {error}");
        return;
    }
    if let Err(error) = ipc.switch_to(plan.target_index) {
        eprintln!("convert: layout switch failed: {error}");
        return;
    }
    if let Err(error) = ipc.wait_for_layout(plan.target_index) {
        eprintln!("convert: layout barrier failed: {error}");
        return;
    }
    if let Err(error) = injector.replay(&plan.replay) {
        eprintln!("convert: replay failed: {error}");
    }
}

/// Publish the converted text, switch by index, wait for the layout-changed
/// event, then paste over the selection. Neither converter advances here;
/// the caller already updated the selection converter.
fn apply_selection_plan(ipc: &mut IpcClient, injector: &mut Injector, plan: &SelectionPlan) {
    if let Err(error) = clipboard::write_selection(&plan.converted) {
        eprintln!("convert selection: clipboard write failed: {error}");
        return;
    }
    if let Err(error) = ipc.switch_to(plan.target_index) {
        eprintln!("convert selection: layout switch failed: {error}");
        return;
    }
    if let Err(error) = ipc.wait_for_layout(plan.target_index) {
        eprintln!("convert selection: layout barrier failed: {error}");
        return;
    }
    if let Err(error) = injector.paste() {
        eprintln!("convert selection: paste failed: {error}");
    }
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

    let mut injector = match Injector::open() {
        Ok(injector) => injector,
        Err(error) => {
            eprintln!("uinput unavailable (/dev/uinput): {error}");
            std::process::exit(1);
        }
    };
    let mut ipc = match IpcClient::connect() {
        Ok(ipc) => ipc,
        Err(error) => {
            eprintln!("niri IPC unavailable ($NIRI_SOCKET): {error}");
            std::process::exit(1);
        }
    };

    let mut reader = Reader::open(&input_dir);
    if reader.live_devices().is_empty() {
        eprintln!("warning: no keyboards open; waiting for hotplug");
    }

    let start = Instant::now();
    let mut triggers = TriggerMachine::new();
    let mut buffer = InputBuffer::default();
    let mut converter = Converter::new();
    let mut selection_converter = SelectionConverter::new();

    loop {
        let Some(raw) = reader.next_key(&input_dir) else {
            eprintln!("input: no devices left, exiting");
            std::process::exit(1);
        };
        let now_ms = start.elapsed().as_millis() as u64;
        let key = reader::classify(raw.scancode);

        if reader::is_typing_key(key, raw.value) {
            // New physical input cancels a pending undo: "repeat" only undoes
            // an immediately preceding conversion.
            converter.invalidate();
            selection_converter.invalidate();
            if raw.value == 1 {
                if reader::is_reset(raw.scancode) {
                    buffer.clear();
                    eprintln!("buffer: cleared (esc)");
                } else {
                    buffer.push(reader::buffer_entry_for(
                        raw.scancode,
                        triggers.shift_held(),
                    ));
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
        let Some(gesture) = triggers.key(key, pressed, now_ms) else {
            continue;
        };
        match gesture.kind {
            GestureKind::Word => {
                if gesture.undo && converter.has_pending_undo() {
                    if let Some(plan) = converter.undo() {
                        eprintln!(
                            "undo: erase {} then replay after layout {}",
                            plan.erase, plan.target_index
                        );
                        apply_plan(&mut ipc, &mut injector, &plan);
                    }
                    continue;
                }
                let word = buffer.trailing_word(reader::is_word_boundary).to_vec();
                let (current, count) = match ipc.current_layout() {
                    Ok(layout) => layout,
                    Err(error) => {
                        eprintln!("convert: layout read failed: {error}");
                        continue;
                    }
                };
                match converter.convert(&word, current, count) {
                    Ok(plan) => {
                        eprintln!(
                            "convert: erase {} then replay after layout {} (from {current})",
                            plan.erase, plan.target_index
                        );
                        // A fresh word conversion supersedes a pending
                        // selection undo: "repeat" only undoes the
                        // immediately preceding conversion.
                        selection_converter.invalidate();
                        apply_plan(&mut ipc, &mut injector, &plan);
                    }
                    Err(error) => eprintln!("convert: skipped ({error})"),
                }
            }
            GestureKind::Phrase => {
                eprintln!("gesture: phrase (wired by ticket 10)");
            }
            GestureKind::Selection => {
                if gesture.undo && selection_converter.has_pending_undo() {
                    if let Some(plan) = selection_converter.undo() {
                        eprintln!(
                            "undo selection: paste back after layout {}",
                            plan.target_index
                        );
                        apply_selection_plan(&mut ipc, &mut injector, &plan);
                    }
                    continue;
                }
                let (current, count) = match ipc.current_layout() {
                    Ok(layout) => layout,
                    Err(error) => {
                        eprintln!("convert selection: layout read failed: {error}");
                        continue;
                    }
                };
                let text = match clipboard::read_selection() {
                    Ok(text) => text,
                    Err(error) => {
                        eprintln!("convert selection: clipboard read failed: {error}");
                        continue;
                    }
                };
                match selection_converter.convert(&text, current, count) {
                    Ok(plan) => {
                        eprintln!(
                            "convert selection: paste {} chars after layout {} (from {current})",
                            plan.converted.chars().count(),
                            plan.target_index
                        );
                        // A fresh selection conversion supersedes a pending
                        // word undo (see the word arm above).
                        converter.invalidate();
                        apply_selection_plan(&mut ipc, &mut injector, &plan);
                    }
                    Err(error) => eprintln!("convert selection: skipped ({error})"),
                }
            }
        }
    }
}
