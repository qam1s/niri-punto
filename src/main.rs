//! niri-punto: keyboard layout corrector daemon for the niri compositor.
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
mod detector;
mod doctor;
mod inject;
mod ipc;
mod keymaps;
mod reader;
// Bigram scorer for direction verdicts behind the detector seam.
mod scorer;
mod selection;
mod setup;
mod trigger;
mod undo;

use buffer::{BufferEntry, InputBuffer};
use clipboard::Clipboard;
use control::{ControlKind, ControlRequest};
use convert::{ConversionPlan, Converter};
use detector::{BigramDetector, Detector};
use inject::{Emitter, Injector};
use ipc::{IpcClient, LayoutBackend};
use reader::Reader;
use scorer::Verdict;
use selection::{SelectionConverter, SelectionPlan};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use trigger::{Gesture, GestureKind, Key, PendingGesture, TriggerMachine};

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
    eprintln!("a lone Mod tap converts the word, Ctrl+Mod tap the selection;");
    eprintln!("niri binds (e.g. Mod+L) use the convert-* subcommands.");
    std::process::exit(2);
}

/// Erase, switch by index, wait for the layout-changed event, then replay.
/// The buffer stays frozen (see [`convert`]); only the [`Converter`] state
/// advances, which the caller already updated. Returns false when any step
/// failed (each failure is already logged); bind replies use it.
fn apply_plan(
    ipc: &mut impl LayoutBackend,
    injector: &mut impl Emitter,
    plan: &ConversionPlan,
) -> bool {
    if let Err(error) = injector.erase(plan.erase) {
        eprintln!("convert: erase failed: {error}");
        return false;
    }
    if let Err(error) = ipc.switch_to(plan.hop.target) {
        eprintln!("convert: layout switch failed: {error}");
        return false;
    }
    if !wait_for_barrier(ipc, plan.hop.target) {
        return false;
    }
    if let Err(error) = injector.replay(&plan.replay) {
        eprintln!("convert: replay failed: {error}");
        return false;
    }
    true
}

/// Wait for the layout barrier, recovering a dead event stream with
/// backoff first (compositor restarts must not kill conversions).
/// When the target is already active the barrier passes without touching
/// the stream: niri emits no event for staying put, so waiting would wedge
/// the daemon mid-conversion with the user's text already erased (a later
/// layout switch would then fire the stale plan over current field text).
/// Returns true when the barrier is satisfied; every failure is logged.
fn wait_for_barrier(ipc: &mut impl LayoutBackend, target: u8) -> bool {
    if let Ok((current, _)) = ipc.current_layout() {
        if current == target {
            return true;
        }
    }
    match ipc.wait_for_layout(target) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("convert: event stream failed ({error}); reconnecting");
            match ipc.recover_barrier(target) {
                Ok(true) => true,
                Ok(false) => {
                    eprintln!("convert: layout {target} not active after reconnect");
                    false
                }
                Err(error) => {
                    eprintln!("convert: event stream recovery failed: {error}");
                    false
                }
            }
        }
    }
}

/// Fresh conversion of `entries`, shared by gestures and bind requests.
/// `phrase` is the whole buffer: the detector verdicts its scope, so an
/// external layout switch between typing and triggering cannot divert the
/// hop target. A confident verdict converts toward the intended layout,
/// except when the layout has not moved since typing and the verdict
/// agrees with it (see [`Converter::convert_toward`]); a decline keeps
/// today's `current_layout` behavior. Returns the one-line detail for
/// daemon logs and socket replies.
fn do_convert(
    entries: &[BufferEntry],
    phrase: &[BufferEntry],
    birth: Option<u8>,
    detector: &dyn Detector,
    converter: &mut Converter,
    ipc: &mut impl LayoutBackend,
    injector: &mut impl Emitter,
) -> Result<String, String> {
    let (current, count) = ipc
        .current_layout()
        .map_err(|error| format!("layout read failed: {error}"))?;
    let plan = match detector.verdict(phrase) {
        Verdict::Intended(intended) => {
            converter.convert_toward(entries, intended, current, count, birth)
        }
        Verdict::Decline => converter.convert(entries, current, count),
    }
    .map_err(|error| format!("skipped ({error})"))?;
    let detail = format!(
        "erase {} then replay after layout {} (from {current})",
        plan.erase, plan.hop.target
    );
    if !apply_plan(ipc, injector, &plan) {
        return Err("erase/switch/replay failed (see daemon log)".to_string());
    }
    Ok(detail)
}

/// Fresh selection conversion, shared by the gesture and bind requests.
/// Like [`do_convert`], a confident verdict on the selection text converts
/// toward the intended layout (hop target and alphabet mapping); a decline
/// keeps today's `current_layout` behavior.
/// Returns the one-line detail for daemon logs and socket replies.
fn do_convert_selection(
    selection_converter: &mut SelectionConverter,
    converter: &mut Converter,
    ipc: &mut impl LayoutBackend,
    injector: &mut impl Emitter,
    clipboard: &mut impl Clipboard,
    detector: &dyn Detector,
) -> Result<String, String> {
    let (current, count) = ipc
        .current_layout()
        .map_err(|error| format!("layout read failed: {error}"))?;
    let text = clipboard
        .read_selection()
        .map_err(|error| format!("clipboard read failed: {error}"))?;
    let plan = match detector.verdict(&scorer::entries_from_text(&text)) {
        Verdict::Intended(intended) => {
            selection_converter.convert_toward(&text, intended, current, count)
        }
        Verdict::Decline => selection_converter.convert(&text, current, count),
    }
    .map_err(|error| format!("skipped ({error})"))?;
    let detail = format!(
        "paste {} chars after layout {} (from {current})",
        plan.converted.chars().count(),
        plan.hop.target
    );
    // A fresh selection conversion supersedes a pending word undo: "repeat"
    // only undoes the immediately preceding conversion.
    converter.invalidate();
    apply_selection_plan(ipc, injector, clipboard, &plan)?;
    Ok(detail)
}

/// Undo the last conversion; `None` when there is nothing to undo.
fn do_undo(
    converter: &mut Converter,
    ipc: &mut impl LayoutBackend,
    injector: &mut impl Emitter,
) -> Option<String> {
    let plan = converter.undo()?;
    let detail = format!(
        "erase {} then replay after layout {}",
        plan.erase, plan.hop.target
    );
    apply_plan(ipc, injector, &plan);
    Some(detail)
}

/// Publish the converted text, switch by index, wait for the layout-changed
/// event, then paste over the selection. Neither converter advances here;
/// the caller already updated the selection converter. Mirrors
/// [`apply_plan`]: each failure is logged and returned, so bind replies
/// report it instead of an unconditional Ok.
fn apply_selection_plan(
    ipc: &mut impl LayoutBackend,
    injector: &mut impl Emitter,
    clipboard: &mut impl Clipboard,
    plan: &SelectionPlan,
) -> Result<(), String> {
    if let Err(error) = clipboard.write_selection(&plan.converted) {
        let detail = format!("clipboard write failed: {error}");
        eprintln!("convert selection: {detail}");
        return Err(detail);
    }
    if let Err(error) = ipc.switch_to(plan.hop.target) {
        let detail = format!("layout switch failed: {error}");
        eprintln!("convert selection: {detail}");
        return Err(detail);
    }
    if !wait_for_barrier(ipc, plan.hop.target) {
        return Err("layout barrier failed (see daemon log)".to_string());
    }
    if let Err(error) = injector.paste() {
        let detail = format!("paste failed: {error}");
        eprintln!("convert selection: {detail}");
        return Err(detail);
    }
    Ok(())
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    // The bare invocation still starts the daemon.
    let (command, rest) = match raw.first().map(String::as_str) {
        None | Some("--input-dir") | Some("--config") => ("run", raw.as_slice()),
        Some("run")
        | Some("convert-word")
        | Some("convert-selection")
        | Some("setup")
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

    let pair = match &config_override {
        Some(path) => config::load_from(path),
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
    let settings = match &config_override {
        Some(path) => config::load_settings_from(path),
        None => config::load_settings(),
    };
    let settings = match settings {
        Ok(settings) => settings,
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
    let mut ipc = match IpcClient::connect_with_retry() {
        Ok(ipc) => ipc,
        Err(error) => {
            eprintln!("niri IPC unavailable ($NIRI_SOCKET): {error}");
            eprintln!(
                "hint: retries ran out; the systemd unit restarts the daemon as a last resort"
            );
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
    triggers.set_tap_action(settings.tap_action);
    triggers.set_binds(settings.binds);
    triggers.set_pair_base(settings.pair_base);
    triggers.set_timing(settings.timing);
    let mut buffer = InputBuffer::default();
    let mut converter = Converter::new(pair.clone());
    let mut selection_converter = SelectionConverter::new(pair);
    // Gestures and bind requests wait here while modifiers are physically
    // held: replaying under a held Shift/Ctrl would render uppercase text
    // (or app shortcuts). Fires on release, see `handle_key`.
    let mut pending: Option<PendingGesture> = None;
    let detector = BigramDetector;
    let mut clipboard = clipboard::WlClipboard;
    // Window-focus tracking: the input buffer belongs to one window (see
    // `note_focus`). `None` until the first successful query.
    let mut last_focus: Option<Option<u64>> = None;
    let mut focus_broken = false;
    eprintln!(
        "detector: {} (direction verdicts, no auto conversion)",
        detector.name()
    );

    loop {
        match reader.poll(&input_dir, CONTROL_POLL) {
            reader::Poll::Gone => {
                // Evdev-missing path: keyboards unplugged or not yet
                // present. Never exit here — hotplug rescans in poll pick
                // up reappearing devices. Sleep to avoid spinning on a
                // disconnected channel. (Distinct from the IPC-missing
                // path above, which retries with backoff and only then
                // exits for systemd to restart.)
                eprintln!("input: no devices left; waiting for hotplug");
                std::thread::sleep(CONTROL_POLL);
            }
            reader::Poll::Idle => {}
            reader::Poll::Key(raw) => {
                let mut daemon = Daemon {
                    triggers: &mut triggers,
                    buffer: &mut buffer,
                    converter: &mut converter,
                    selection_converter: &mut selection_converter,
                    detector: &detector,
                    ipc: &mut ipc,
                    injector: &mut injector,
                    clipboard: &mut clipboard,
                };
                note_focus(
                    &mut daemon,
                    &mut last_focus,
                    &mut focus_broken,
                    &mut pending,
                );
                handle_key(raw, &start, &mut daemon, &mut pending);
            }
        }
        while let Ok(request) = ctrl_rx.try_recv() {
            let mut daemon = Daemon {
                triggers: &mut triggers,
                buffer: &mut buffer,
                converter: &mut converter,
                selection_converter: &mut selection_converter,
                detector: &detector,
                ipc: &mut ipc,
                injector: &mut injector,
                clipboard: &mut clipboard,
            };
            let now_ms = start.elapsed().as_millis() as u64;
            note_focus(
                &mut daemon,
                &mut last_focus,
                &mut focus_broken,
                &mut pending,
            );
            handle_control(request, now_ms, &mut daemon, &mut pending);
        }
    }
}

/// One niri-bind request: convert now, or stage into the wait-for-release
/// slot while modifiers are physically held. A bind fires with its own
/// modifier still down (Mod in Mod+L): replaying under it would render
/// uppercase text or app shortcuts, so it waits like a gesture, bound by
/// the same timeout.
fn handle_control(
    request: ControlRequest,
    now_ms: u64,
    daemon: &mut Daemon<impl Emitter, impl LayoutBackend, impl Clipboard>,
    pending: &mut Option<PendingGesture>,
) {
    let Daemon {
        triggers,
        buffer,
        converter,
        selection_converter,
        detector,
        ipc,
        injector,
        clipboard,
    } = daemon;
    if !triggers.modifiers_free() {
        let kind = match request.kind {
            ControlKind::Word => GestureKind::Word,
            ControlKind::Selection => GestureKind::Selection,
        };
        *pending = Some(triggers.stage(Gesture { kind, undo: false }, now_ms));
        eprintln!(
            "control: {} deferred until modifiers release",
            request.kind.as_line().trim()
        );
        let _ = request
            .reply
            .send("ok deferred until modifiers release".to_string());
        return;
    }
    let answer = match request.kind {
        ControlKind::Word => {
            let word = buffer.trailing_word(reader::is_word_boundary).to_vec();
            match do_convert(
                &word,
                buffer.phrase(),
                buffer.birth_layout(),
                *detector,
                converter,
                &mut **ipc,
                &mut **injector,
            ) {
                Ok(detail) => {
                    // Bind conversions supersede a pending selection
                    // undo, same as the gesture path.
                    selection_converter.invalidate();
                    if let Some(target) = converter.last_target() {
                        buffer.reanchor(target);
                    }
                    format!("ok bind-word: {detail}")
                }
                Err(detail) => format!("err bind-word: {detail}"),
            }
        }
        ControlKind::Selection => {
            match do_convert_selection(
                selection_converter,
                converter,
                &mut **ipc,
                &mut **injector,
                &mut **clipboard,
                *detector,
            ) {
                Ok(detail) => {
                    if let Some(target) = selection_converter.last_target() {
                        buffer.reanchor(target);
                    }
                    format!("ok bind-selection: {detail}")
                }
                Err(detail) => format!("err bind-selection: {detail}"),
            }
        }
    };
    eprintln!("control: {} -> {answer}", request.kind.as_line().trim());
    let _ = request.reply.send(answer);
}

/// Mutable daemon state threaded through key handling (keeps `handle_key`
/// within the argument-count lint). The detector verdicts the conversion
/// direction at the gesture decision point (ADR-0006): confident verdicts
/// set the hop target, declines keep the `current_layout` behavior.
/// `I`/`L`/`C` are the hardware seams (uinput, niri IPC, clipboard):
/// production passes the real types, tests pass fakes.
struct Daemon<'a, I: Emitter, L: LayoutBackend, C: Clipboard> {
    triggers: &'a mut TriggerMachine,
    buffer: &'a mut InputBuffer,
    converter: &'a mut Converter,
    selection_converter: &'a mut SelectionConverter,
    detector: &'a dyn Detector,
    ipc: &'a mut L,
    injector: &'a mut I,
    clipboard: &'a mut C,
}

/// Window-focus tracking: the input buffer belongs to one window, so a
/// focus change clears it, cancels pending undos, and drops a staged
/// gesture (the same "context moved on" rule as new physical input).
/// Without this, typing in the browser then triggering in the terminal
/// replays foreign scancodes there. Runs before each key event and each
/// niri-bind request. A failed query keeps the old state; outages log
/// once until queries succeed again.
fn note_focus(
    daemon: &mut Daemon<impl Emitter, impl LayoutBackend, impl Clipboard>,
    last_focus: &mut Option<Option<u64>>,
    focus_broken: &mut bool,
    pending: &mut Option<PendingGesture>,
) {
    match daemon.ipc.focused_window_id() {
        Ok(focus) => {
            *focus_broken = false;
            if let Some(old) = last_focus.replace(focus) {
                if old != focus {
                    daemon.buffer.clear();
                    daemon.converter.invalidate();
                    daemon.selection_converter.invalidate();
                    *pending = None;
                    eprintln!("focus: window changed ({old:?} -> {focus:?}), buffer cleared");
                }
            }
        }
        Err(error) => {
            if !*focus_broken {
                *focus_broken = true;
                eprintln!("focus: query failed ({error}); keeping buffer");
            }
        }
    }
}

fn handle_key(
    raw: reader::RawKey,
    start: &Instant,
    daemon: &mut Daemon<impl Emitter, impl LayoutBackend, impl Clipboard>,
    pending: &mut Option<PendingGesture>,
) {
    handle_key_at(raw, start.elapsed().as_millis() as u64, daemon, pending);
}

/// Key handling at an explicit timestamp. Production passes wall-clock time
/// via [`handle_key`]; tests pass scripted times to stay clear of the
/// 30 ms debounce window without sleeping.
fn handle_key_at(
    raw: reader::RawKey,
    now_ms: u64,
    daemon: &mut Daemon<impl Emitter, impl LayoutBackend, impl Clipboard>,
    pending: &mut Option<PendingGesture>,
) {
    let Daemon {
        triggers,
        buffer,
        converter,
        selection_converter,
        detector,
        ipc,
        injector,
        clipboard,
    } = daemon;
    let key = reader::classify(raw.scancode);

    // Daemon-side binds (config `binds` block): the bind key is the
    // trigger, not text — unlike niri-bind keys it skips the buffer, so
    // the converted scope stays exact.
    let bind = (raw.value == 1)
        .then(|| triggers.bind(raw.scancode, now_ms))
        .flatten();
    if let Some(gesture) = bind {
        // Latest gesture wins: it supersedes anything still waiting.
        *pending = Some(triggers.stage(gesture, now_ms));
    } else if reader::is_typing_key(key, raw.value) {
        // New physical input cancels a pending undo: "repeat" only undoes
        // an immediately preceding conversion. It also cancels a gesture
        // still waiting for modifiers: the context moved on.
        converter.invalidate();
        selection_converter.invalidate();
        *pending = None;
        // Anchor the fill to the layout it is typed under (one IPC query
        // per fill): a confident verdict only overrides the current layout
        // when the layout moved since typing (external switch). Edits and
        // cursor moves carry no layout of their own, so they never anchor.
        if !reader::is_backspace(raw.scancode)
            && !reader::is_navigation(raw.scancode)
            && buffer.birth_layout().is_none()
        {
            if let Ok((current, _)) = ipc.current_layout() {
                buffer.note_birth(current);
            }
        }
        if reader::is_backspace(raw.scancode) {
            // Mirror the screen edit instead of recording it: a plain
            // Backspace drops one entry per press and per autorepeat,
            // Ctrl+Backspace drops the trailing word. Recording the
            // Backspace scancode itself desyncs erase/replay from the
            // visible text (a stale prefix on the next conversion).
            if triggers.ctrl_held() {
                buffer.pop_word(reader::is_word_boundary);
            } else {
                buffer.pop();
            }
        } else if reader::is_navigation(raw.scancode) {
            // The cursor moved (or text changed ahead of it): the buffer
            // no longer describes the screen, so drop it rather than
            // corrupt the next conversion. Refusing beats mangling.
            buffer.clear();
            if raw.value == 1 {
                eprintln!("buffer: cleared (cursor moved)");
            }
        } else if raw.value == 1 {
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
    if let Some(gesture) = triggers.key(key, pressed, now_ms) {
        // Latest gesture wins: it supersedes anything still waiting.
        // A gesture completed on a lone Mod release is a tap: only meta()
        // returns Some on those events, so the key tells the source.
        let mut staged = triggers.stage(gesture, now_ms);
        staged.tap = !pressed && matches!(key, Key::MetaLeft | Key::MetaRight);
        *pending = Some(staged);
    }
    let Some(staged) = pending.take() else {
        return;
    };
    if staged.expired(now_ms) {
        eprintln!("convert: gesture expired with modifiers held; tap again");
        return;
    }
    if !staged.ready(now_ms, triggers.modifiers_free()) {
        // Shift/Ctrl still physically held: replaying now would render
        // uppercase text (or app shortcuts for Ctrl). Wait for release.
        *pending = Some(staged);
        return;
    }
    // Detector seam (ADR-0006): the verdict is consulted inside
    // `do_convert` / `do_convert_selection`, which pick the hop target.
    match staged.kind {
        GestureKind::Word => {
            if staged.undo && converter.has_pending_undo() {
                if let Some(detail) = do_undo(converter, &mut **ipc, &mut **injector) {
                    if let Some(target) = converter.last_target() {
                        buffer.reanchor(target);
                    }
                    eprintln!("undo: {detail}");
                }
                return;
            }
            if staged.tap && buffer.phrase().is_empty() {
                // Lone Mod tap with no text: toggle the layout within the
                // pair instead of converting. Repeat toggles back, so no
                // undo bookkeeping is needed.
                match ipc.current_layout() {
                    Ok((current, _)) => {
                        let target = if current == 0 { 1 } else { 0 };
                        match ipc.switch_to(target) {
                            Ok(()) => eprintln!("tap: no text, switched to layout {target}"),
                            Err(error) => eprintln!("tap: layout switch failed: {error}"),
                        }
                    }
                    Err(error) => eprintln!("tap: layout read failed: {error}"),
                }
                return;
            }
            let word = buffer.trailing_word(reader::is_word_boundary).to_vec();
            match do_convert(
                &word,
                buffer.phrase(),
                buffer.birth_layout(),
                *detector,
                converter,
                &mut **ipc,
                &mut **injector,
            ) {
                Ok(detail) => {
                    // A fresh word conversion supersedes a pending
                    // selection undo: "repeat" only undoes the
                    // immediately preceding conversion.
                    selection_converter.invalidate();
                    if let Some(target) = converter.last_target() {
                        buffer.reanchor(target);
                    }
                    eprintln!("convert: {detail}");
                }
                Err(detail) => eprintln!("convert: {detail}"),
            }
        }
        GestureKind::Phrase => {
            if staged.undo && converter.has_pending_undo() {
                if let Some(detail) = do_undo(converter, &mut **ipc, &mut **injector) {
                    if let Some(target) = converter.last_target() {
                        buffer.reanchor(target);
                    }
                    eprintln!("undo: {detail}");
                }
                return;
            }
            let phrase = buffer.phrase().to_vec();
            match do_convert(
                &phrase,
                &phrase,
                buffer.birth_layout(),
                *detector,
                converter,
                &mut **ipc,
                &mut **injector,
            ) {
                Ok(detail) => {
                    if let Some(target) = converter.last_target() {
                        buffer.reanchor(target);
                    }
                    eprintln!("phrase: {detail}")
                }
                Err(detail) => eprintln!("phrase: {detail}"),
            }
        }
        GestureKind::Selection => {
            if staged.undo && selection_converter.has_pending_undo() {
                if let Some(plan) = selection_converter.undo() {
                    eprintln!(
                        "undo selection: paste back after layout {}",
                        plan.hop.target
                    );
                    match apply_selection_plan(&mut **ipc, &mut **injector, &mut **clipboard, &plan)
                    {
                        Ok(()) => buffer.reanchor(plan.hop.target),
                        Err(detail) => eprintln!("undo selection: {detail}"),
                    }
                }
                return;
            }
            match do_convert_selection(
                selection_converter,
                converter,
                &mut **ipc,
                &mut **injector,
                &mut **clipboard,
                *detector,
            ) {
                Ok(detail) => {
                    if let Some(target) = selection_converter.last_target() {
                        buffer.reanchor(target);
                    }
                    eprintln!("convert selection: {detail}")
                }
                Err(detail) => eprintln!("convert selection: {detail}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::BufferEntry;
    use crate::config::LayoutPair;
    use crate::detector::ManualOnly;
    use crate::trigger::{Bind, ModSet};
    use evdev::KeyCode;
    use std::io;

    /// Recording uinput stand-in: no `/dev/uinput`, just the call log.
    #[derive(Default)]
    struct FakeEmitter {
        erases: Vec<usize>,
        replays: Vec<Vec<BufferEntry>>,
        pastes: usize,
    }

    impl Emitter for FakeEmitter {
        fn erase(&mut self, count: usize) -> io::Result<()> {
            self.erases.push(count);
            Ok(())
        }

        fn replay(&mut self, entries: &[BufferEntry]) -> io::Result<()> {
            self.replays.push(entries.to_vec());
            Ok(())
        }

        fn paste(&mut self) -> io::Result<()> {
            self.pastes += 1;
            Ok(())
        }
    }

    /// Niri stand-in: no `$NIRI_SOCKET`. Switches flip the reported index,
    /// so the layout barrier passes immediately.
    struct FakeLayouts {
        current: u8,
        count: usize,
        switches: Vec<u8>,
        wait_calls: usize,
        stream_dead: bool,
        focus: Option<u64>,
        focus_err: bool,
    }

    impl LayoutBackend for FakeLayouts {
        fn current_layout(&mut self) -> io::Result<(u8, usize)> {
            Ok((self.current, self.count))
        }

        fn switch_to(&mut self, index: u8) -> io::Result<()> {
            self.switches.push(index);
            self.current = index;
            Ok(())
        }

        fn wait_for_layout(&mut self, _target: u8) -> io::Result<()> {
            self.wait_calls += 1;
            if self.stream_dead {
                return Err(io::Error::other("stream dead"));
            }
            Ok(())
        }

        fn recover_barrier(&mut self, target: u8) -> io::Result<bool> {
            if self.stream_dead {
                return Err(io::Error::other("stream dead"));
            }
            Ok(self.current == target)
        }

        fn focused_window_id(&mut self) -> io::Result<Option<u64>> {
            if self.focus_err {
                return Err(io::Error::other("focus query dead"));
            }
            Ok(self.focus)
        }
    }

    /// In-memory clipboard: no Wayland, no `wl-copy`/`wl-paste`.
    struct FakeClipboard {
        text: String,
        written: Vec<String>,
    }

    impl Clipboard for FakeClipboard {
        fn read_selection(&mut self) -> io::Result<String> {
            Ok(self.text.clone())
        }

        fn write_selection(&mut self, text: &str) -> io::Result<()> {
            self.written.push(text.to_string());
            Ok(())
        }
    }

    const T: u64 = 10_000;
    const SHIFT: u16 = KeyCode::KEY_LEFTSHIFT.code();
    const SHIFT_RIGHT: u16 = KeyCode::KEY_RIGHTSHIFT.code();
    const CTRL: u16 = KeyCode::KEY_LEFTCTRL.code();
    const BACKSPACE: u16 = KeyCode::KEY_BACKSPACE.code();
    const LEFT: u16 = KeyCode::KEY_LEFT.code();
    /// Mod (Super): held during a Mod+L bind, invisible to the trigger
    /// machine until the fix.
    const SUPER: u16 = KeyCode::KEY_LEFTMETA.code();
    /// `ghbdtn`-shaped word: six scancodes, layout-agnostic.
    const WORD: [u16; 6] = [34, 35, 32, 48, 49, 20];

    struct Harness {
        triggers: TriggerMachine,
        buffer: InputBuffer,
        converter: Converter,
        selection_converter: SelectionConverter,
        detector: Box<dyn Detector>,
        ipc: FakeLayouts,
        injector: FakeEmitter,
        clipboard: FakeClipboard,
        pending: Option<PendingGesture>,
        now: u64,
        last_focus: Option<Option<u64>>,
        focus_broken: bool,
    }

    impl Harness {
        fn new() -> Self {
            Self::with_detector(Box::new(ManualOnly))
        }

        fn with_detector(detector: Box<dyn Detector>) -> Self {
            let pair = LayoutPair::new("us", "ru").unwrap();
            Self::with_detector_and_pair(detector, pair)
        }

        fn with_detector_and_pair(detector: Box<dyn Detector>, pair: LayoutPair) -> Self {
            Self {
                triggers: TriggerMachine::new(),
                buffer: InputBuffer::default(),
                converter: Converter::new(pair.clone()),
                selection_converter: SelectionConverter::new(pair),
                detector,
                ipc: FakeLayouts {
                    current: 0,
                    count: 2,
                    switches: Vec::new(),
                    wait_calls: 0,
                    stream_dead: false,
                    focus: Some(7),
                    focus_err: false,
                },
                injector: FakeEmitter::default(),
                clipboard: FakeClipboard {
                    text: "ghbdtn".to_string(),
                    written: Vec::new(),
                },
                pending: None,
                now: T,
                last_focus: None,
                focus_broken: false,
            }
        }

        fn key(&mut self, scancode: u16, value: i32) {
            let raw = reader::RawKey { scancode, value };
            let mut daemon = Daemon {
                triggers: &mut self.triggers,
                buffer: &mut self.buffer,
                converter: &mut self.converter,
                selection_converter: &mut self.selection_converter,
                detector: self.detector.as_ref(),
                ipc: &mut self.ipc,
                injector: &mut self.injector,
                clipboard: &mut self.clipboard,
            };
            handle_key_at(raw, self.now, &mut daemon, &mut self.pending);
        }

        /// Run the production focus guard: same call the run loop makes
        /// before each key event and each niri-bind request.
        fn check_focus(&mut self) {
            let mut daemon = Daemon {
                triggers: &mut self.triggers,
                buffer: &mut self.buffer,
                converter: &mut self.converter,
                selection_converter: &mut self.selection_converter,
                detector: self.detector.as_ref(),
                ipc: &mut self.ipc,
                injector: &mut self.injector,
                clipboard: &mut self.clipboard,
            };
            note_focus(
                &mut daemon,
                &mut self.last_focus,
                &mut self.focus_broken,
                &mut self.pending,
            );
        }

        fn at(&mut self, now: u64, scancode: u16, value: i32) {
            self.now = now;
            self.key(scancode, value);
        }

        fn tap(&mut self, now: u64, scancode: u16) {
            self.at(now, scancode, 1);
            self.at(now + 10, scancode, 0);
        }

        fn type_word(&mut self, start: u64) {
            for (i, code) in WORD.iter().enumerate() {
                let t = start + (i as u64) * 20;
                self.at(t, *code, 1);
                self.at(t + 10, *code, 0);
            }
        }

        fn type_codes(&mut self, start: u64, codes: &[u16]) {
            for (i, code) in codes.iter().enumerate() {
                let t = start + (i as u64) * 20;
                self.at(t, *code, 1);
                self.at(t + 10, *code, 0);
            }
        }

        /// Double Shift with trigger-test spacing (past the 30 ms debounce,
        /// inside the 400 ms pair window). Fires on the release at `t + 200`.
        fn double_shift(&mut self, t: u64) {
            self.at(t, SHIFT, 1);
            self.at(t + 50, SHIFT, 0);
            self.at(t + 150, SHIFT, 1);
            self.at(t + 200, SHIFT, 0);
        }
    }

    #[test]
    fn barrier_already_at_target_skips_the_event_stream() {
        // No-op hop (a confident verdict agreeing with the current
        // layout): niri emits no event for staying put, so waiting would
        // wedge the daemon mid-conversion with the user's text erased.
        // The stream is poisoned here: any touch fails, recovery included.
        let mut h = Harness::new();
        h.ipc.stream_dead = true;
        assert!(wait_for_barrier(&mut h.ipc, 0));
        assert_eq!(h.ipc.wait_calls, 0);
    }

    #[test]
    fn barrier_real_switch_still_waits_on_the_stream() {
        let mut h = Harness::new();
        assert!(wait_for_barrier(&mut h.ipc, 1));
        assert_eq!(h.ipc.wait_calls, 1);
    }

    #[test]
    fn focus_change_clears_buffer_cancels_undo_and_drops_pending() {
        use crate::trigger::{Gesture, GestureKind};

        let mut h = Harness::new();
        h.type_word(T);
        h.check_focus();
        assert!(!h.buffer.phrase().is_empty());
        // A conversion leaves a pending undo behind.
        h.double_shift(T + 500);
        assert!(h.converter.has_pending_undo());
        // A staged gesture waits for release.
        h.pending = Some(h.triggers.stage(
            Gesture {
                kind: GestureKind::Word,
                undo: false,
            },
            h.now,
        ));
        // Same window: everything survives.
        h.check_focus();
        assert!(!h.buffer.phrase().is_empty());
        assert!(h.converter.has_pending_undo());
        assert!(h.pending.is_some());
        // Another window: buffer cleared, undo cancelled, staged dropped.
        h.ipc.focus = Some(99);
        h.check_focus();
        assert!(h.buffer.phrase().is_empty());
        assert!(!h.converter.has_pending_undo());
        assert!(h.pending.is_none());
    }

    #[test]
    fn focus_query_error_keeps_buffer_until_recovery() {
        let mut h = Harness::new();
        h.type_word(T);
        h.check_focus();
        h.ipc.focus_err = true;
        h.check_focus();
        assert!(!h.buffer.phrase().is_empty());
        // Recovery on the same window keeps the buffer too.
        h.ipc.focus_err = false;
        h.check_focus();
        assert!(!h.buffer.phrase().is_empty());
    }

    #[test]
    fn word_gesture_erases_switches_and_replays() {
        let mut h = Harness::new();
        h.type_word(T);
        h.double_shift(T + 500);
        assert_eq!(h.ipc.switches, vec![1]);
        assert_eq!(h.injector.erases, vec![6]);
        assert_eq!(h.injector.replays.len(), 1);
        assert_eq!(
            h.injector.replays[0]
                .iter()
                .map(|e| e.scancode)
                .collect::<Vec<_>>(),
            WORD.to_vec()
        );
        assert!(h.converter.has_pending_undo());
    }

    #[test]
    fn backspace_then_retype_converts_only_retyped_word() {
        // The reported case: type a letter, erase it, type a new word.
        // The stale prefix must not leak into erase/replay counts.
        let mut h = Harness::new();
        h.tap(T, 38);
        h.tap(T + 100, BACKSPACE);
        assert!(h.buffer.phrase().is_empty());
        h.type_codes(T + 200, &[34, 23, 20]);
        h.double_shift(T + 800);
        assert_eq!(h.ipc.switches, vec![1]);
        assert_eq!(h.injector.erases, vec![3]);
        assert_eq!(
            h.injector.replays[0]
                .iter()
                .map(|e| e.scancode)
                .collect::<Vec<_>>(),
            vec![34, 23, 20]
        );
    }

    #[test]
    fn backspace_autorepeat_pops_one_entry_per_repeat() {
        let mut h = Harness::new();
        h.type_codes(T, &[30, 48, 31, 32]);
        h.at(T + 500, BACKSPACE, 1);
        h.at(T + 510, BACKSPACE, 0);
        h.at(T + 520, BACKSPACE, 2);
        h.at(T + 530, BACKSPACE, 2);
        let codes: Vec<u16> = h.buffer.phrase().iter().map(|e| e.scancode).collect();
        assert_eq!(codes, vec![30]);
    }

    #[test]
    fn ctrl_backspace_drops_the_trailing_word() {
        let mut h = Harness::new();
        h.type_codes(T, &[30, 48, 57, 31, 32]);
        h.at(T + 500, CTRL, 1);
        h.at(T + 520, BACKSPACE, 1);
        h.at(T + 530, BACKSPACE, 0);
        h.at(T + 540, CTRL, 0);
        let codes: Vec<u16> = h.buffer.phrase().iter().map(|e| e.scancode).collect();
        assert_eq!(codes, vec![30, 48, 57]);
        // The trailing word is empty now: the conversion is refused
        // instead of replaying a stale suffix.
        h.double_shift(T + 900);
        assert!(h.injector.erases.is_empty());
        assert!(h.ipc.switches.is_empty());
    }

    #[test]
    fn arrow_clears_the_buffer() {
        let mut h = Harness::new();
        h.type_word(T);
        assert!(!h.buffer.phrase().is_empty());
        h.tap(T + 500, LEFT);
        assert!(h.buffer.phrase().is_empty());
    }

    /// `ghbdtn` in true keystroke order (g,h,b,d,t,n): Ru-confident for
    /// the verdict tests. (WORD above is layout-agnostic replay order,
    /// which scores differently — bigrams are order-sensitive.)
    const GHBTDN: [u16; 6] = [34, 35, 48, 32, 20, 49];

    #[test]
    fn diverged_layout_confident_verdict_converts_toward_intended() {
        // 18:55 scenario: typed in us, external switch to ru, trigger.
        // The confident RU verdict converts toward ru instead of toggling
        // back to us (today's ManualOnly behavior, asserted below).
        let mut h = Harness::with_detector(Box::new(BigramDetector));
        h.type_codes(T, &GHBTDN);
        h.ipc.current = 1;
        h.double_shift(T + 500);
        assert_eq!(h.ipc.switches, vec![1]);
        assert_eq!(h.injector.erases, vec![6]);
        assert_eq!(
            h.injector.replays[0]
                .iter()
                .map(|e| e.scancode)
                .collect::<Vec<_>>(),
            GHBTDN.to_vec()
        );

        let mut today = Harness::new();
        today.type_codes(T, &GHBTDN);
        today.ipc.current = 1;
        today.double_shift(T + 500);
        assert_eq!(today.ipc.switches, vec![0]);
    }

    #[test]
    fn unchanged_layout_agreeing_verdict_flips_for_convert_away() {
        // Typed under ru, still ru, RU verdict for `ghbdtn` keystrokes: no
        // external switch happened, so the user converts away — mechanical
        // flip to us instead of a no-op hop and no-op undos.
        let mut h = Harness::with_detector(Box::new(BigramDetector));
        h.ipc.current = 1;
        h.type_codes(T, &GHBTDN);
        h.at(T + 500, SUPER, 1);
        h.at(T + 600, SUPER, 0);
        assert_eq!(h.ipc.switches, vec![0]);
        assert_eq!(h.injector.erases, vec![6]);
    }

    #[test]
    fn second_word_after_a_flip_reanchors_and_converts_away() {
        // Word 1 flips us->ru; the frozen buffer must not keep the stale
        // birth: word 2 (typed under ru, RU verdict, current ru) flips
        // back instead of flapping a no-op hop.
        let mut h = Harness::with_detector(Box::new(BigramDetector));
        h.type_codes(T, &GHBTDN);
        h.double_shift(T + 500);
        assert_eq!(h.ipc.switches, vec![1]);
        // Space plus word 2 under the new layout (typing also retires the
        // pending undo, so the tap below converts fresh).
        h.at(T + 2000, KeyCode::KEY_SPACE.code(), 1);
        h.at(T + 2010, KeyCode::KEY_SPACE.code(), 0);
        h.type_codes(T + 2100, &GHBTDN);
        h.at(T + 3000, SUPER, 1);
        h.at(T + 3100, SUPER, 0);
        assert_eq!(h.ipc.switches, vec![1, 0]);
        assert_eq!(h.injector.erases, vec![6, 6]);
    }

    #[test]
    fn diverged_layout_decline_matches_today_byte_identical() {
        // "hi" declines (below the margin): the bigram path must erase,
        // switch, and replay exactly what ManualOnly does today.
        let mut bigram = Harness::with_detector(Box::new(BigramDetector));
        let mut manual = Harness::new();
        for h in [&mut bigram, &mut manual] {
            h.type_codes(T, &[35, 23]); // h, i
            h.ipc.current = 1;
            h.double_shift(T + 500);
        }
        assert_eq!(bigram.ipc.switches, vec![0]);
        assert_eq!(bigram.ipc.switches, manual.ipc.switches);
        assert_eq!(bigram.injector.erases, manual.injector.erases);
        assert_eq!(bigram.injector.replays, manual.injector.replays);
    }

    #[test]
    fn diverged_layout_confident_selection_converts_toward_intended() {
        // Clipboard holds us-typed "ghbdtn" while ru is current: the
        // confident RU verdict maps EN->RU and hops to ru.
        let mut h = Harness::with_detector(Box::new(BigramDetector));
        h.ipc.current = 1;
        h.at(T, CTRL, 1);
        h.at(T + 50, SHIFT, 1);
        h.at(T + 100, SHIFT, 0);
        h.at(T + 200, SHIFT, 1);
        h.at(T + 250, SHIFT, 0);
        assert!(h.ipc.switches.is_empty());
        h.at(T + 300, CTRL, 0);
        assert_eq!(h.ipc.switches, vec![1]);
        assert_eq!(h.clipboard.written, vec!["привет".to_string()]);
        assert_eq!(h.injector.pastes, 1);
    }

    #[test]
    fn swapped_pair_diverged_word_converts_toward_the_ru_position() {
        // `layout ru,us`: the confident RU verdict for `ghbdtn` keystrokes
        // hops to position 0 even with the diverged current at 1, so the
        // text and the indicator land together.
        let pair = LayoutPair::new("ru", "us").unwrap();
        let mut h = Harness::with_detector_and_pair(Box::new(BigramDetector), pair);
        h.type_codes(T, &GHBTDN);
        h.ipc.current = 1;
        h.double_shift(T + 500);
        assert_eq!(h.ipc.switches, vec![0]);
        assert_eq!(h.injector.erases, vec![6]);
    }

    #[test]
    fn swapped_pair_diverged_selection_converts_toward_the_ru_position() {
        // `layout ru,us`: clipboard "ghbdtn" with current at 1 verdicts RU,
        // writes Cyrillic, and hops to position 0.
        let pair = LayoutPair::new("ru", "us").unwrap();
        let mut h = Harness::with_detector_and_pair(Box::new(BigramDetector), pair);
        h.ipc.current = 1;
        h.at(T, CTRL, 1);
        h.at(T + 50, SHIFT, 1);
        h.at(T + 100, SHIFT, 0);
        h.at(T + 200, SHIFT, 1);
        h.at(T + 250, SHIFT, 0);
        assert!(h.ipc.switches.is_empty());
        h.at(T + 300, CTRL, 0);
        assert_eq!(h.ipc.switches, vec![0]);
        assert_eq!(h.clipboard.written, vec!["привет".to_string()]);
        assert_eq!(h.injector.pastes, 1);
    }

    #[test]
    fn repeated_gesture_undoes() {
        let mut h = Harness::new();
        h.type_word(T);
        h.double_shift(T + 500);
        h.double_shift(T + 900);
        assert_eq!(h.ipc.switches, vec![1, 0]);
        assert_eq!(h.injector.erases, vec![6, 6]);
        assert_eq!(h.injector.replays.len(), 2);
    }

    #[test]
    fn typing_between_cancels_undo_and_converts_fresh() {
        let mut h = Harness::new();
        h.type_word(T);
        h.double_shift(T + 500);
        // Buffer freezes after conversion, so the trailing word is 6 + 1.
        h.tap(T + 900, 30);
        h.double_shift(T + 1100);
        // Fresh 7-key word converts from the new layout back.
        assert_eq!(h.ipc.switches, vec![1, 0]);
        assert_eq!(h.injector.erases, vec![6, 7]);
    }

    #[test]
    fn empty_word_converts_nothing() {
        let mut h = Harness::new();
        h.double_shift(T);
        assert!(h.ipc.switches.is_empty());
        assert!(h.injector.erases.is_empty());
        assert!(h.injector.replays.is_empty());
    }

    #[test]
    fn ctrl_double_shift_converts_selection() {
        let mut h = Harness::new();
        h.at(T, CTRL, 1);
        h.at(T + 50, SHIFT, 1);
        h.at(T + 100, SHIFT, 0);
        h.at(T + 200, SHIFT, 1);
        h.at(T + 250, SHIFT, 0);
        // Ctrl still held: the gesture waits for release.
        assert!(h.ipc.switches.is_empty());
        h.at(T + 300, CTRL, 0);
        assert_eq!(h.ipc.switches, vec![1]);
        assert_eq!(h.clipboard.written, vec!["привет".to_string()]);
        assert_eq!(h.injector.pastes, 1);
    }

    #[test]
    fn bind_word_while_super_held_defers_until_release() {
        let mut h = Harness::new();
        h.type_word(T);
        h.at(T + 500, SUPER, 1); // Mod held, as in Mod+L
        let now = h.now;
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let mut daemon = Daemon {
                triggers: &mut h.triggers,
                buffer: &mut h.buffer,
                converter: &mut h.converter,
                selection_converter: &mut h.selection_converter,
                detector: h.detector.as_ref(),
                ipc: &mut h.ipc,
                injector: &mut h.injector,
                clipboard: &mut h.clipboard,
            };
            handle_control(
                ControlRequest {
                    kind: ControlKind::Word,
                    reply: tx,
                },
                now,
                &mut daemon,
                &mut h.pending,
            );
        }
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "ok deferred until modifiers release"
        );
        assert!(
            h.injector.erases.is_empty(),
            "replayed while Mod held: {:?}",
            h.injector.erases
        );
        assert!(h.ipc.switches.is_empty());
        h.at(T + 600, SUPER, 0); // release Mod: the staged request fires
        assert_eq!(h.ipc.switches, vec![1]);
        assert_eq!(h.injector.erases, vec![6]);
    }

    fn mod_l(m: &mut TriggerMachine) {
        m.set_binds(vec![Bind {
            mods: ModSet {
                meta: true,
                ..Default::default()
            },
            scancode: 38,
            kind: GestureKind::Word,
        }]);
    }

    #[test]
    fn mod_shift_chord_converts_phrase_on_mod_release() {
        // Mod first, then Shift: the chord stages on the Shift press and
        // converts the whole buffer once Mod is released (replaying under
        // a held Mod would land in the compositor binds).
        let mut h = Harness::new();
        h.type_word(T);
        h.at(T + 500, SUPER, 1);
        h.at(T + 550, SHIFT, 1);
        assert!(h.ipc.switches.is_empty());
        h.at(T + 600, SHIFT, 0);
        assert!(h.ipc.switches.is_empty());
        h.at(T + 650, SUPER, 0);
        assert_eq!(h.ipc.switches, vec![1]);
        assert_eq!(h.injector.erases, vec![6]);
    }

    #[test]
    fn bind_converts_word_with_exact_scope() {
        let mut h = Harness::new();
        mod_l(&mut h.triggers);
        h.type_word(T);
        h.at(T + 500, SUPER, 1);
        h.at(T + 550, 38, 1);
        h.at(T + 560, 38, 0);
        // Meta still held: staged, nothing fires yet.
        assert!(h.ipc.switches.is_empty());
        h.at(T + 600, SUPER, 0);
        assert_eq!(h.ipc.switches, vec![1]);
        // The chord key is the trigger, not text: exactly the typed word.
        assert_eq!(h.injector.erases, vec![6]);
    }

    #[test]
    fn repeated_bind_undoes() {
        let mut h = Harness::new();
        mod_l(&mut h.triggers);
        h.type_word(T);
        h.at(T + 500, SUPER, 1);
        h.at(T + 550, 38, 1);
        h.at(T + 560, 38, 0);
        h.at(T + 600, SUPER, 0);
        h.at(T + 900, SUPER, 1);
        h.at(T + 950, 38, 1);
        h.at(T + 960, 38, 0);
        h.at(T + 1000, SUPER, 0);
        assert_eq!(h.ipc.switches, vec![1, 0]);
        assert_eq!(h.injector.erases, vec![6, 6]);
    }

    #[test]
    fn empty_tap_toggles_layout() {
        let mut h = Harness::new();
        h.at(T, SUPER, 1);
        h.at(T + 100, SUPER, 0);
        assert_eq!(h.ipc.switches, vec![1]);
        assert!(h.injector.erases.is_empty());
        // Repeat toggles back.
        h.at(T + 500, SUPER, 1);
        h.at(T + 600, SUPER, 0);
        assert_eq!(h.ipc.switches, vec![1, 0]);
    }

    #[test]
    fn tap_with_only_a_trailing_space_does_not_toggle() {
        let mut h = Harness::new();
        h.tap(T, KeyCode::KEY_SPACE.code());
        h.at(T + 500, SUPER, 1);
        h.at(T + 600, SUPER, 0);
        assert!(h.ipc.switches.is_empty());
        assert!(h.injector.erases.is_empty());
    }

    #[test]
    fn mod_tap_converts_word() {
        let mut h = Harness::new();
        h.type_word(T);
        h.at(T + 500, SUPER, 1);
        h.at(T + 600, SUPER, 0);
        assert_eq!(h.ipc.switches, vec![1]);
        assert_eq!(h.injector.erases, vec![6]);
        assert_eq!(h.injector.replays.len(), 1);
        assert!(h.converter.has_pending_undo());
    }

    #[test]
    fn repeated_mod_tap_undoes() {
        let mut h = Harness::new();
        h.type_word(T);
        h.at(T + 500, SUPER, 1);
        h.at(T + 600, SUPER, 0);
        h.at(T + 900, SUPER, 1);
        h.at(T + 1000, SUPER, 0);
        assert_eq!(h.ipc.switches, vec![1, 0]);
        assert_eq!(h.injector.erases, vec![6, 6]);
    }

    #[test]
    fn held_shift_double_shift_converts_phrase() {
        let mut h = Harness::new();
        h.type_word(T);
        // Phrase gesture: second Shift side while the first stays held.
        h.at(T + 500, SHIFT, 1);
        h.at(T + 550, SHIFT_RIGHT, 1);
        h.at(T + 600, SHIFT_RIGHT, 0);
        assert!(h.ipc.switches.is_empty());
        h.at(T + 650, SHIFT, 0);
        assert_eq!(h.ipc.switches, vec![1]);
        assert_eq!(h.injector.erases, vec![6]);
    }
}
