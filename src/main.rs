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
//! Ticket 10 (phrase + binds): Shift+DoubleShift converts the phrase (whole
//! buffer) through shared convert helpers, and the `run` daemon also serves
//! `convert-word` / `convert-selection` requests from niri binds over its
//! control socket.
//! Ticket 11 (setup + doctor): `setup` installs the binary, user unit,
//! default KDL config, and the udev rule; `doctor` checks devices,
//! permissions, socket, and layouts; the daemon loads the layout pair.

mod buffer;
mod clipboard;
mod config;
mod control;
mod convert;
mod doctor;
mod inject;
mod ipc;
mod keymaps;
mod reader;
mod selection;
mod setup;
mod trigger;

use buffer::{BufferEntry, InputBuffer};
use control::{ControlKind, ControlRequest};
use convert::{ConversionPlan, Converter};
use inject::Injector;
use ipc::IpcClient;
use reader::Reader;
use selection::{SelectionConverter, SelectionPlan};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use trigger::{GestureKind, TriggerMachine};

/// How often the main loop wakes to serve control-socket bind requests.
const CONTROL_POLL: Duration = Duration::from_millis(100);

fn usage() -> ! {
    eprintln!("usage: niri-punto <command> [options]");
    eprintln!("  run [--input-dir DIR] [--config PATH]  start the daemon (single instance)");
    eprintln!("  convert-word           ask the running daemon to convert the last word");
    eprintln!("  convert-selection      ask the running daemon to convert the selection");
    eprintln!("  setup [--no-udev] [--dry-run]  install binary, unit, config, udev rule");
    eprintln!("  doctor                 check devices, permissions, socket, layouts");
    eprintln!("Double Shift converts the word, Shift+DoubleShift the phrase;");
    eprintln!("niri binds (e.g. Mod+L) use the convert-* subcommands.");
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

/// Fresh selection conversion, shared by the gesture and bind requests.
/// Returns the one-line detail for daemon logs and socket replies.
fn do_convert_selection(
    selection_converter: &mut SelectionConverter,
    converter: &mut Converter,
    ipc: &mut IpcClient,
    injector: &mut Injector,
) -> Result<String, String> {
    let (current, count) = ipc
        .current_layout()
        .map_err(|error| format!("layout read failed: {error}"))?;
    let text =
        clipboard::read_selection().map_err(|error| format!("clipboard read failed: {error}"))?;
    let plan = selection_converter
        .convert(&text, current, count)
        .map_err(|error| format!("skipped ({error})"))?;
    let detail = format!(
        "paste {} chars after layout {} (from {current})",
        plan.converted.chars().count(),
        plan.target_index
    );
    // A fresh selection conversion supersedes a pending word undo: "repeat"
    // only undoes the immediately preceding conversion.
    converter.invalidate();
    apply_selection_plan(ipc, injector, &plan);
    Ok(detail)
}

/// Undo the last conversion; `None` when there is nothing to undo.
fn do_undo(
    converter: &mut Converter,
    ipc: &mut IpcClient,
    injector: &mut Injector,
) -> Option<String> {
    let plan = converter.undo()?;
    let detail = format!(
        "erase {} then replay after layout {}",
        plan.erase, plan.target_index
    );
    apply_plan(ipc, injector, &plan);
    Some(detail)
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
    let raw: Vec<String> = std::env::args().skip(1).collect();
    // The bare invocation still starts the daemon.
    let (command, rest) = match raw.first().map(String::as_str) {
        None | Some("--input-dir") | Some("--config") => ("run", raw.as_slice()),
        Some("run") | Some("convert-word") | Some("convert-selection") | Some("setup")
        | Some("doctor") => (raw[0].as_str(), &raw[1..]),
        _ => usage(),
    };
    match command {
        "setup" => {
            let mut options = setup::Options::default();
            for arg in rest {
                match arg.as_str() {
                    "--no-udev" => options.no_udev = true,
                    "--dry-run" => options.dry_run = true,
                    _ => usage(),
                }
            }
            let paths = match setup::resolve() {
                Ok(paths) => paths,
                Err(error) => {
                    eprintln!("setup: {error}");
                    std::process::exit(1);
                }
            };
            std::process::exit(setup::run(options, &paths, &setup::RealRunner));
        }
        "doctor" => {
            if !rest.is_empty() {
                usage();
            }
            std::process::exit(doctor::run());
        }
        "run" => {
            let mut input_dir = PathBuf::from("/dev/input");
            let mut config_override: Option<PathBuf> = None;
            let mut rest = rest.iter();
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--input-dir" => {
                        input_dir = rest.next().map(PathBuf::from).unwrap_or_else(|| usage());
                    }
                    "--config" => {
                        config_override =
                            Some(rest.next().map(PathBuf::from).unwrap_or_else(|| usage()));
                    }
                    _ => usage(),
                }
            }
            run(input_dir, config_override);
        }
        "convert-word" => {
            if !rest.is_empty() {
                usage();
            }
            client(ControlKind::Word);
        }
        "convert-selection" => {
            if !rest.is_empty() {
                usage();
            }
            client(ControlKind::Selection);
        }
        _ => usage(),
    }
}

/// One-shot client for niri binds: send the request to the running daemon
/// and report its one-line reply. Never starts a daemon.
fn client(kind: ControlKind) {
    let path = control::socket_path();
    match control::send(&path, kind) {
        Ok(reply) if reply == "ok" || reply.starts_with("ok ") => {
            println!("{reply}");
        }
        Ok(reply) => {
            eprintln!("{reply}");
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

fn run(input_dir: PathBuf, config_override: Option<PathBuf>) {
    // Single-instance guard first: a live socket means another daemon owns
    // the input, so exit before touching hardware.
    let socket_path = control::socket_path();
    let listener = match control::bind(&socket_path) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    eprintln!("control: listening on {}", socket_path.display());
    let (ctrl_tx, ctrl_rx) = mpsc::channel::<ControlRequest>();
    if std::thread::Builder::new()
        .name("control".to_string())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        control::serve_one(stream, &ctrl_tx);
                    }
                    Err(_) => break,
                }
            }
        })
        .is_err()
    {
        eprintln!("control: listener thread failed; binds will not work");
    }

    let pair = match config_override {
        Some(path) => config::load_from(&path),
        None => config::load(),
    };
    let pair = match pair {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("config: {error}");
            eprintln!("hint: run `niri-punto setup` to write the default config");
            std::process::exit(1);
        }
    };

    let mut injector = match Injector::open() {
        Ok(injector) => injector,
        Err(error) => {
            eprintln!("uinput unavailable (/dev/uinput): {error}");
            let _ = std::fs::remove_file(&socket_path);
            std::process::exit(1);
        }
    };
    let mut ipc = match IpcClient::connect() {
        Ok(ipc) => ipc,
        Err(error) => {
            eprintln!("niri IPC unavailable ($NIRI_SOCKET): {error}");
            let _ = std::fs::remove_file(&socket_path);
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
    let mut converter = Converter::new(pair);
    let mut selection_converter = SelectionConverter::new(pair.clone());

    loop {
        match reader.poll(&input_dir, CONTROL_POLL) {
            reader::Poll::Gone => {
                eprintln!("input: no devices left, exiting");
                let _ = std::fs::remove_file(&socket_path);
                std::process::exit(1);
            }
            reader::Poll::Idle => {}
            reader::Poll::Key(raw) => {
                let mut daemon = Daemon {
                    triggers: &mut triggers,
                    buffer: &mut buffer,
                    converter: &mut converter,
                    selection_converter: &mut selection_converter,
                    ipc: &mut ipc,
                    injector: &mut injector,
                };
                handle_key(raw, &start, &mut daemon);
            }
        }
        while let Ok(request) = ctrl_rx.try_recv() {
            let answer = match request.kind {
                ControlKind::Word => {
                    let word = buffer.trailing_word(reader::is_word_boundary).to_vec();
                    match do_convert(&word, &mut converter, &mut ipc, &mut injector) {
                        Ok(detail) => {
                            // Bind conversions supersede a pending selection
                            // undo, same as the gesture path.
                            selection_converter.invalidate();
                            format!("ok bind-word: {detail}")
                        }
                        Err(detail) => format!("err bind-word: {detail}"),
                    }
                }
                ControlKind::Selection => {
                    match do_convert_selection(
                        &mut selection_converter,
                        &mut converter,
                        &mut ipc,
                        &mut injector,
                    ) {
                        Ok(detail) => format!("ok bind-selection: {detail}"),
                        Err(detail) => format!("err bind-selection: {detail}"),
                    }
                }
            };
            eprintln!("control: {} -> {answer}", request.kind.as_line().trim());
            let _ = request.reply.send(answer);
        }
    }
}

/// Mutable daemon state threaded through key handling (keeps `handle_key`
/// within the argument-count lint).
struct Daemon<'a> {
    triggers: &'a mut TriggerMachine,
    buffer: &'a mut InputBuffer,
    converter: &'a mut Converter,
    selection_converter: &'a mut SelectionConverter,
    ipc: &'a mut IpcClient,
    injector: &'a mut Injector,
}

fn handle_key(raw: reader::RawKey, start: &Instant, daemon: &mut Daemon) {
    let Daemon {
        triggers,
        buffer,
        converter,
        selection_converter,
        ipc,
        injector,
    } = daemon;
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
        return;
    };
    match gesture.kind {
        GestureKind::Word => {
            if gesture.undo && converter.has_pending_undo() {
                if let Some(detail) = do_undo(converter, ipc, injector) {
                    eprintln!("undo: {detail}");
                }
                return;
            }
            let word = buffer.trailing_word(reader::is_word_boundary).to_vec();
            match do_convert(&word, converter, ipc, injector) {
                Ok(detail) => {
                    // A fresh word conversion supersedes a pending
                    // selection undo: "repeat" only undoes the
                    // immediately preceding conversion.
                    selection_converter.invalidate();
                    eprintln!("convert: {detail}");
                }
                Err(detail) => eprintln!("convert: {detail}"),
            }
        }
        GestureKind::Phrase => {
            if gesture.undo && converter.has_pending_undo() {
                if let Some(detail) = do_undo(converter, ipc, injector) {
                    eprintln!("undo: {detail}");
                }
                return;
            }
            let phrase = buffer.phrase().to_vec();
            match do_convert(&phrase, converter, ipc, injector) {
                Ok(detail) => eprintln!("phrase: {detail}"),
                Err(detail) => eprintln!("phrase: {detail}"),
            }
        }
        GestureKind::Selection => {
            if gesture.undo && selection_converter.has_pending_undo() {
                if let Some(plan) = selection_converter.undo() {
                    eprintln!(
                        "undo selection: paste back after layout {}",
                        plan.target_index
                    );
                    apply_selection_plan(ipc, injector, &plan);
                }
                return;
            }
            match do_convert_selection(selection_converter, converter, ipc, injector) {
                Ok(detail) => eprintln!("convert selection: {detail}"),
                Err(detail) => eprintln!("convert selection: {detail}"),
            }
        }
    }
}
