//! niri-punto: Punto Switcher-style daemon for the niri compositor.
//!
//! Ticket 10 (phrase + binds): Double Shift converts the last word,
//! Shift+DoubleShift converts the phrase (whole buffer), and the `run`
//! daemon also serves `convert-word` / `convert-selection` requests from
//! niri binds over its control socket. Repeating a gesture undoes the
//! conversion. Selection conversion is another ticket's lane: the protocol
//! accepts it but the daemon answers "not wired yet".

mod buffer;
mod convert;
mod inject;
mod ipc;
mod reader;
mod trigger;

use buffer::{BufferEntry, InputBuffer};
use convert::{ConversionPlan, Converter};
use inject::Injector;
use ipc::IpcClient;
use reader::Reader;
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
/// advances, which the caller already updated. Returns false when any step
/// failed (each failure is already logged); bind replies use it.
fn apply_plan(ipc: &mut IpcClient, injector: &mut Injector, plan: &ConversionPlan) -> bool {
    if let Err(error) = injector.erase(plan.erase) {
        eprintln!("convert: erase failed: {error}");
        return false;
    }
    if let Err(error) = ipc.switch_to(plan.target_index) {
        eprintln!("convert: layout switch failed: {error}");
        return false;
    }
    if let Err(error) = ipc.wait_for_layout(plan.target_index) {
        eprintln!("convert: layout barrier failed: {error}");
        return false;
    }
    if let Err(error) = injector.replay(&plan.replay) {
        eprintln!("convert: replay failed: {error}");
        return false;
    }
    true
}

/// Fresh conversion of `entries`, shared by gestures and bind requests.
/// Returns the one-line detail for daemon logs and socket replies.
fn do_convert(
    entries: &[BufferEntry],
    converter: &mut Converter,
    ipc: &mut IpcClient,
    injector: &mut Injector,
) -> Result<String, String> {
    let (current, count) = ipc
        .current_layout()
        .map_err(|error| format!("layout read failed: {error}"))?;
    let plan = converter
        .convert(entries, current, count)
        .map_err(|error| format!("skipped ({error})"))?;
    let detail = format!(
        "erase {} then replay after layout {} (from {current})",
        plan.erase, plan.target_index
    );
    if !apply_plan(ipc, injector, &plan) {
        return Err("erase/switch/replay failed (see daemon log)".to_string());
    }
    Ok(detail)
}

/// Undo the last conversion; `None` when there is nothing to undo.
fn do_undo(converter: &mut Converter, ipc: &mut IpcClient, injector: &mut Injector) -> Option<String> {
    let plan = converter.undo()?;
    let detail = format!(
        "erase {} then replay after layout {}",
        plan.erase, plan.target_index
    );
    apply_plan(ipc, injector, &plan);
    Some(detail)
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
                    if let Some(detail) = do_undo(&mut converter, &mut ipc, &mut injector) {
                        eprintln!("undo: {detail}");
                    }
                    continue;
                }
                let word = buffer.trailing_word(reader::is_word_boundary).to_vec();
                match do_convert(&word, &mut converter, &mut ipc, &mut injector) {
                    Ok(detail) => eprintln!("convert: {detail}"),
                    Err(detail) => eprintln!("convert: {detail}"),
                }
            }
            GestureKind::Phrase => {
                if gesture.undo && converter.has_pending_undo() {
                    if let Some(detail) = do_undo(&mut converter, &mut ipc, &mut injector) {
                        eprintln!("undo: {detail}");
                    }
                    continue;
                }
                let phrase = buffer.phrase().to_vec();
                match do_convert(&phrase, &mut converter, &mut ipc, &mut injector) {
                    Ok(detail) => eprintln!("phrase: {detail}"),
                    Err(detail) => eprintln!("phrase: {detail}"),
                }
            }
            GestureKind::Selection => {
                eprintln!("gesture: selection (wired by ticket 09)");
            }
        }
    }
}
