//! Double Shift trigger state machine.
//!
//! Pure logic: a stream of timestamped key presses and releases goes in,
//! a recognized [`Gesture`] comes out. No I/O, no clock reads — the caller
//! supplies `now_ms`, which keeps the machine unit-testable without hardware.

/// Maximum gap between the two Shift presses of one trigger.
pub const DOUBLE_SHIFT_WINDOW_MS: u64 = 400;
/// A repeated gesture inside this window after a previous one means undo.
pub const UNDO_WINDOW_MS: u64 = 3000;
/// Presses faster than this after a release are switch bounce, not intent.
pub const DEBOUNCE_MS: u64 = 30;
/// A lone Mod press released within this window is a tap gesture. Longer
/// holds are chord prefixes (or hesitation), never a tap.
pub const TAP_WINDOW_MS: u64 = 300;

/// All trigger timing knobs in one place. From the config `timings`
/// block; absent means [`TimingConfig::default`], which is these consts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TimingConfig {
    pub double_shift_ms: u64,
    pub undo_ms: u64,
    pub debounce_ms: u64,
    pub pending_ms: u64,
    pub tap_ms: u64,
}

impl Default for TimingConfig {
    fn default() -> Self {
        Self {
            double_shift_ms: DOUBLE_SHIFT_WINDOW_MS,
            undo_ms: UNDO_WINDOW_MS,
            debounce_ms: DEBOUNCE_MS,
            pending_ms: PENDING_TIMEOUT_MS,
            tap_ms: TAP_WINDOW_MS,
        }
    }
}

/// Keys the machine cares about. Everything else is [`Key::Other`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    ShiftLeft,
    ShiftRight,
    CtrlLeft,
    CtrlRight,
    MetaLeft,
    MetaRight,
    Other,
}

/// Modifier set of a daemon-side bind, in niri spelling (`Mod` is Super).
/// At least one modifier is required; Alt is rejected at config load
/// until the machine tracks it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ModSet {
    pub meta: bool,
    pub shift: bool,
    pub ctrl: bool,
}

impl ModSet {
    pub fn any(self) -> bool {
        self.meta || self.shift || self.ctrl
    }
}

/// A daemon-side bind from the config `binds` block: the modifiers held
/// plus the key press convert with the given scope.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Bind {
    pub mods: ModSet,
    pub scancode: u16,
    pub kind: GestureKind,
}

/// Which conversion scope a trigger asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GestureKind {
    /// Double Shift: convert the last word.
    Word,
    /// Shift held + Double Shift: convert the phrase.
    Phrase,
    /// Ctrl held + Double Shift: convert the current selection.
    Selection,
}

/// A recognized trigger.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Gesture {
    pub kind: GestureKind,
    /// True when the same gesture repeats right after a previous one.
    pub undo: bool,
}

/// How long a recognized gesture waits for a modifier-free moment before
/// it is dropped. Covers slow Shift releases; bounds the window in which a
/// focus change could redirect the conversion.
pub const PENDING_TIMEOUT_MS: u64 = 2000;

/// A gesture waiting for a modifier-free moment to run. Conversions must
/// never replay while Shift/Ctrl is physically held: the held modifier
/// would combine with the replayed scancodes (uppercase text, or app
/// shortcuts for Ctrl), so the daemon fires these on release instead.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PendingGesture {
    pub kind: GestureKind,
    pub undo: bool,
    /// True when the gesture completed on a lone Mod release. The Word
    /// branch reads it: a tap with no text toggles the layout instead of
    /// converting.
    pub tap: bool,
    deadline_ms: u64,
}

impl PendingGesture {
    pub fn new(gesture: Gesture, now_ms: u64, timeout_ms: u64) -> Self {
        Self {
            kind: gesture.kind,
            undo: gesture.undo,
            tap: false,
            deadline_ms: now_ms.saturating_add(timeout_ms),
        }
    }

    /// Ready when no modifier is held and the wait has not expired.
    pub fn ready(&self, now_ms: u64, modifiers_free: bool) -> bool {
        modifiers_free && now_ms <= self.deadline_ms
    }

    /// Too long since recognition (e.g. stuck modifier, focus moved on):
    /// the caller drops it instead of converting stale context.
    pub fn expired(&self, now_ms: u64) -> bool {
        now_ms > self.deadline_ms
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Left,
    Right,
}

impl Side {
    fn index(self) -> usize {
        match self {
            Side::Left => 0,
            Side::Right => 1,
        }
    }
}

/// Recognizes Double Shift gestures and lone Mod taps from a key event
/// stream.
///
/// Feed every press and release in order via [`TriggerMachine::key`]; it
/// returns `Some(Gesture)` exactly at the second Shift press that completes
/// a trigger, or at the Mod release that completes a tap — `None`
/// otherwise. Autorepeat (press while already down) and bounce faster than
/// [`DEBOUNCE_MS`] are ignored. Any non-modifier press between the two
/// Shift presses cancels the pending pair; a tap needs a clean
/// press-to-release with no Shift, text, or second-Mod key in between.
pub struct TriggerMachine {
    shift_down: [bool; 2],
    ctrl_down: [bool; 2],
    meta_down: [bool; 2],
    first_press_ms: Option<u64>,
    disturbed: bool,
    last_release_ms: [Option<u64>; 2],
    meta_release_ms: [Option<u64>; 2],
    tap_press_ms: Option<u64>,
    tap_disturbed: bool,
    /// Lone-Mod-tap scope from a bare-`Mod` bind; `None` disables the tap.
    tap_action: Option<GestureKind>,
    /// Plain-pair scope from the `double-shift` node (modified pairs keep
    /// phrase/selection).
    pair_base: GestureKind,
    binds: Vec<Bind>,
    timing: TimingConfig,
    last_gesture: Option<(GestureKind, u64)>,
}

impl TriggerMachine {
    pub fn new() -> Self {
        Self {
            shift_down: [false, false],
            ctrl_down: [false, false],
            meta_down: [false, false],
            first_press_ms: None,
            disturbed: false,
            last_release_ms: [None, None],
            meta_release_ms: [None, None],
            tap_press_ms: None,
            tap_disturbed: false,
            tap_action: Some(GestureKind::Word),
            pair_base: GestureKind::Word,
            binds: Vec::new(),
            timing: TimingConfig::default(),
            last_gesture: None,
        }
    }

    /// Shift currently held on either side (for buffer entry recording).
    pub fn shift_held(&self) -> bool {
        self.shift_down[0] || self.shift_down[1]
    }

    /// Ctrl currently held on either side (for selection gestures).
    pub fn ctrl_held(&self) -> bool {
        self.ctrl_down[0] || self.ctrl_down[1]
    }

    /// Mod (Super) currently held on either side. Tracked so a bind like
    /// Mod+L waits for release: replaying scancodes under a held Mod lands
    /// in the compositor's binds instead of the text field.
    pub fn meta_held(&self) -> bool {
        self.meta_down[0] || self.meta_down[1]
    }

    /// Lone-Mod-tap scope from a bare-`Mod` bind; `None` disables the tap.
    pub fn set_tap_action(&mut self, tap_action: Option<GestureKind>) {
        self.tap_action = tap_action;
    }

    /// Plain-pair scope from the `double-shift` node.
    pub fn set_pair_base(&mut self, pair_base: GestureKind) {
        self.pair_base = pair_base;
    }

    /// Daemon-side binds from the config (`binds` block).
    pub fn set_binds(&mut self, binds: Vec<Bind>) {
        self.binds = binds;
    }

    /// Timing knobs from the config (`timings` block).
    pub fn set_timing(&mut self, timing: TimingConfig) {
        self.timing = timing;
    }

    /// Stage a recognized gesture into the wait-for-release slot with the
    /// configured pending timeout.
    pub fn stage(&self, gesture: Gesture, now_ms: u64) -> PendingGesture {
        PendingGesture::new(gesture, now_ms, self.timing.pending_ms)
    }

    /// A configured bind key pressed with its modifiers held. The caller
    /// checks press (not autorepeat) first; required modifiers must all be
    /// held, extra ones are allowed. Undo accounting rides the shared
    /// gesture chain, like taps.
    pub fn bind(&mut self, scancode: u16, now_ms: u64) -> Option<Gesture> {
        let kind = self
            .binds
            .iter()
            .find(|bind| bind.scancode == scancode && self.mods_match(bind.mods))?
            .kind;
        let undo = matches!(self.last_gesture, Some((k, t)) if k == kind && now_ms.saturating_sub(t) <= self.timing.undo_ms);
        self.last_gesture = Some((kind, now_ms));
        Some(Gesture { kind, undo })
    }

    fn mods_match(&self, mods: ModSet) -> bool {
        (!mods.meta || self.meta_held())
            && (!mods.shift || self.shift_held())
            && (!mods.ctrl || self.ctrl_held())
    }

    /// No Shift, Ctrl, or Mod held anywhere: replaying scancodes now renders
    /// exactly what the buffer holds, with no held-modifier interference.
    pub fn modifiers_free(&self) -> bool {
        !self.shift_held() && !self.ctrl_held() && !self.meta_held()
    }

    pub fn key(&mut self, key: Key, pressed: bool, now_ms: u64) -> Option<Gesture> {
        match key {
            Key::ShiftLeft => self.shift(Side::Left, pressed, now_ms),
            Key::ShiftRight => self.shift(Side::Right, pressed, now_ms),
            Key::CtrlLeft => {
                self.ctrl_down[Side::Left.index()] = pressed;
                None
            }
            Key::CtrlRight => {
                self.ctrl_down[Side::Right.index()] = pressed;
                None
            }
            Key::MetaLeft => self.meta(Side::Left, pressed, now_ms),
            Key::MetaRight => self.meta(Side::Right, pressed, now_ms),
            Key::Other => {
                if pressed {
                    self.disturbed = true;
                    self.tap_disturbed = true;
                }
                None
            }
        }
    }

    fn shift(&mut self, side: Side, pressed: bool, now_ms: u64) -> Option<Gesture> {
        let i = side.index();
        if pressed {
            // A Shift chord voids a Mod tap; the tap never voids a pair.
            self.tap_disturbed = true;
            if self.shift_down[i] {
                return None; // autorepeat, not a new press
            }
            if let Some(released) = self.last_release_ms[i]
                && now_ms.saturating_sub(released) < self.timing.debounce_ms
            {
                return None; // switch bounce
            }
            self.shift_down[i] = true;
            // Mod+Shift chord (Mod first): phrase without a bind — a bare
            // Mod+Shift has no letter key, so `binds` cannot express it.
            // Ctrl narrows to Selection, mirroring the pair precedence
            // below. Converts on Mod release via the pending slot.
            if self.meta_held() {
                self.first_press_ms = None;
                self.disturbed = false;
                let kind = if self.ctrl_held() {
                    GestureKind::Selection
                } else {
                    GestureKind::Phrase
                };
                let undo = matches!(self.last_gesture, Some((k, t)) if k == kind && now_ms.saturating_sub(t) <= self.timing.undo_ms);
                self.last_gesture = Some((kind, now_ms));
                return Some(Gesture { kind, undo });
            }
            match self.first_press_ms {
                Some(first)
                    if !self.disturbed
                        && now_ms.saturating_sub(first) <= self.timing.double_shift_ms =>
                {
                    self.first_press_ms = None;
                    self.disturbed = false;
                    let kind = if self.ctrl_held() {
                        GestureKind::Selection
                    } else if self.other_side_down(side) {
                        GestureKind::Phrase
                    } else {
                        self.pair_base
                    };
                    let undo = matches!(self.last_gesture, Some((k, t)) if k == kind && now_ms.saturating_sub(t) <= self.timing.undo_ms);
                    self.last_gesture = Some((kind, now_ms));
                    Some(Gesture { kind, undo })
                }
                _ => {
                    // No usable pending first press: start a new pair.
                    self.first_press_ms = Some(now_ms);
                    self.disturbed = false;
                    None
                }
            }
        } else {
            self.shift_down[i] = false;
            self.last_release_ms[i] = Some(now_ms);
            None
        }
    }

    /// A lone Mod tap: press and release within [`TAP_WINDOW_MS`] with no
    /// Shift, text, or second-Mod key in between, and no Shift held at
    /// release. Completes on release (modifiers are free by definition,
    /// except a held Ctrl — that resolves to Selection and the caller
    /// defers it). Ctrl modifies like in a pair and never cancels; a Mod
    /// press still disturbs a pending Shift pair, as before.
    fn meta(&mut self, side: Side, pressed: bool, now_ms: u64) -> Option<Gesture> {
        let i = side.index();
        if pressed {
            if self.meta_down[i] {
                return None; // autorepeat, not a new press
            }
            if let Some(released) = self.meta_release_ms[i]
                && now_ms.saturating_sub(released) < self.timing.debounce_ms
            {
                return None; // switch bounce
            }
            self.meta_down[i] = true;
            self.tap_press_ms = Some(now_ms);
            // The other side already down means a chord, not a tap.
            self.tap_disturbed = self.meta_down[1 - i];
            self.disturbed = true;
            None
        } else {
            self.meta_down[i] = false;
            self.meta_release_ms[i] = Some(now_ms);
            let tap = match self.tap_action {
                None => None,
                Some(base) => match self.tap_press_ms {
                    Some(pressed_at)
                        if !self.tap_disturbed
                            && !self.shift_held()
                            && now_ms.saturating_sub(pressed_at) <= self.timing.tap_ms =>
                    {
                        let kind = if self.ctrl_held() {
                            GestureKind::Selection
                        } else {
                            base
                        };
                        let undo = matches!(self.last_gesture, Some((k, t)) if k == kind && now_ms.saturating_sub(t) <= self.timing.undo_ms);
                        self.last_gesture = Some((kind, now_ms));
                        Some(Gesture { kind, undo })
                    }
                    _ => None,
                },
            };
            self.tap_press_ms = None;
            self.tap_disturbed = false;
            tap
        }
    }

    fn other_side_down(&self, side: Side) -> bool {
        match side {
            Side::Left => self.shift_down[Side::Right.index()],
            Side::Right => self.shift_down[Side::Left.index()],
        }
    }
}

impl Default for TriggerMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 10_000;

    fn press(m: &mut TriggerMachine, key: Key, now_ms: u64) -> Option<Gesture> {
        m.key(key, true, now_ms)
    }

    fn release(m: &mut TriggerMachine, key: Key, now_ms: u64) -> Option<Gesture> {
        m.key(key, false, now_ms)
    }

    fn double_shift(m: &mut TriggerMachine, at: u64) -> Option<Gesture> {
        assert_eq!(press(m, Key::ShiftLeft, at), None);
        assert_eq!(release(m, Key::ShiftLeft, at + 50), None);
        press(m, Key::ShiftLeft, at + 150)
    }

    #[test]
    fn single_press_is_not_a_gesture() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::ShiftLeft, T), None);
        assert_eq!(release(&mut m, Key::ShiftLeft, T + 50), None);
    }

    #[test]
    fn double_shift_same_side_is_word() {
        let mut m = TriggerMachine::new();
        let g = double_shift(&mut m, T);
        assert_eq!(
            g,
            Some(Gesture {
                kind: GestureKind::Word,
                undo: false
            })
        );
    }

    #[test]
    fn double_shift_mixed_sides_is_word() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::ShiftLeft, T), None);
        assert_eq!(release(&mut m, Key::ShiftLeft, T + 50), None);
        let g = press(&mut m, Key::ShiftRight, T + 150);
        assert_eq!(
            g,
            Some(Gesture {
                kind: GestureKind::Word,
                undo: false
            })
        );
    }

    #[test]
    fn slow_second_press_starts_a_new_pair() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::ShiftLeft, T), None);
        assert_eq!(release(&mut m, Key::ShiftLeft, T + 50), None);
        // Past the window: a new first press, no gesture.
        assert_eq!(
            press(&mut m, Key::ShiftLeft, T + DOUBLE_SHIFT_WINDOW_MS + 100),
            None
        );
        assert_eq!(
            release(&mut m, Key::ShiftLeft, T + DOUBLE_SHIFT_WINDOW_MS + 150),
            None
        );
        // Completing the new pair gestures.
        let g = press(&mut m, Key::ShiftLeft, T + DOUBLE_SHIFT_WINDOW_MS + 250);
        assert!(matches!(
            g,
            Some(Gesture {
                kind: GestureKind::Word,
                undo: false
            })
        ));
    }

    #[test]
    fn held_shift_turns_double_shift_into_phrase() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::ShiftLeft, T), None);
        // Second press on the other side while the first is still held.
        let g = press(&mut m, Key::ShiftRight, T + 120);
        assert_eq!(
            g,
            Some(Gesture {
                kind: GestureKind::Phrase,
                undo: false
            })
        );
    }

    #[test]
    fn held_ctrl_turns_double_shift_into_selection() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::CtrlLeft, T), None);
        let g = double_shift(&mut m, T + 50);
        assert_eq!(
            g,
            Some(Gesture {
                kind: GestureKind::Selection,
                undo: false
            })
        );
        assert_eq!(release(&mut m, Key::CtrlLeft, T + 500), None);
    }

    #[test]
    fn ctrl_beats_held_shift() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::CtrlLeft, T), None);
        assert_eq!(press(&mut m, Key::ShiftLeft, T + 10), None);
        let g = press(&mut m, Key::ShiftRight, T + 100);
        assert!(matches!(
            g,
            Some(Gesture {
                kind: GestureKind::Selection,
                ..
            })
        ));
    }

    #[test]
    fn repeated_gesture_is_undo() {
        let mut m = TriggerMachine::new();
        let first = double_shift(&mut m, T);
        assert!(matches!(first, Some(Gesture { undo: false, .. })));
        release(&mut m, Key::ShiftLeft, T + 200);
        let second = double_shift(&mut m, T + 500);
        assert_eq!(
            second,
            Some(Gesture {
                kind: GestureKind::Word,
                undo: true
            })
        );
    }

    #[test]
    fn different_gesture_is_not_undo_but_resets_chain() {
        let mut m = TriggerMachine::new();
        let first = double_shift(&mut m, T);
        assert!(matches!(first, Some(Gesture { undo: false, .. })));
        release(&mut m, Key::ShiftLeft, T + 200);
        // Phrase gesture after a word gesture: no undo.
        assert_eq!(press(&mut m, Key::ShiftLeft, T + 500), None);
        let phrase = press(&mut m, Key::ShiftRight, T + 600);
        assert_eq!(
            phrase,
            Some(Gesture {
                kind: GestureKind::Phrase,
                undo: false
            })
        );
    }

    #[test]
    fn undo_window_expiry_clears_undo() {
        let mut m = TriggerMachine::new();
        let first = double_shift(&mut m, T);
        assert!(matches!(first, Some(Gesture { undo: false, .. })));
        release(&mut m, Key::ShiftLeft, T + 200);
        let late = double_shift(&mut m, T + UNDO_WINDOW_MS + 1000);
        assert_eq!(
            late,
            Some(Gesture {
                kind: GestureKind::Word,
                undo: false
            })
        );
    }

    #[test]
    fn typing_between_shifts_cancels_the_pair() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::ShiftLeft, T), None);
        assert_eq!(release(&mut m, Key::ShiftLeft, T + 50), None);
        assert_eq!(press(&mut m, Key::Other, T + 100), None);
        assert_eq!(release(&mut m, Key::Other, T + 130), None);
        // The second Shift press starts a fresh pair instead of gesturing.
        assert_eq!(press(&mut m, Key::ShiftLeft, T + 150), None);
    }

    #[test]
    fn modifier_presses_do_not_cancel_the_pair() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::ShiftLeft, T), None);
        assert_eq!(release(&mut m, Key::ShiftLeft, T + 50), None);
        assert_eq!(press(&mut m, Key::CtrlLeft, T + 100), None);
        assert_eq!(release(&mut m, Key::CtrlLeft, T + 130), None);
        // Ctrl was released again: still a plain word gesture.
        let g = press(&mut m, Key::ShiftLeft, T + 150);
        assert!(matches!(
            g,
            Some(Gesture {
                kind: GestureKind::Word,
                undo: false
            })
        ));
    }

    #[test]
    fn autorepeat_press_is_ignored() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::ShiftLeft, T), None);
        // Repeat without release: ignored, and must not complete a pair.
        assert_eq!(press(&mut m, Key::ShiftLeft, T + 100), None);
        assert_eq!(release(&mut m, Key::ShiftLeft, T + 150), None);
        // Next real press is past the window, so it starts a fresh pair.
        assert_eq!(press(&mut m, Key::ShiftLeft, T + 600), None);
    }

    #[test]
    fn bounce_after_release_is_ignored() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::ShiftLeft, T), None);
        assert_eq!(release(&mut m, Key::ShiftLeft, T + 50), None);
        // Bounce within the debounce window: ignored entirely.
        assert_eq!(
            press(&mut m, Key::ShiftLeft, T + 50 + DEBOUNCE_MS - 1),
            None
        );
        assert_eq!(release(&mut m, Key::ShiftLeft, T + 50 + DEBOUNCE_MS), None);
        // A real press after the debounce window starts a fresh pair.
        assert_eq!(press(&mut m, Key::ShiftLeft, T + 500), None);
    }

    #[test]
    fn releases_alone_never_gesture() {
        let mut m = TriggerMachine::new();
        assert_eq!(release(&mut m, Key::ShiftLeft, T), None);
        assert_eq!(release(&mut m, Key::ShiftRight, T + 10), None);
        assert_eq!(release(&mut m, Key::Other, T + 20), None);
    }

    fn mod_tap(m: &mut TriggerMachine, at: u64) -> Option<Gesture> {
        assert_eq!(press(m, Key::MetaLeft, at), None);
        release(m, Key::MetaLeft, at + 100)
    }

    #[test]
    fn mod_tap_is_word() {
        let mut m = TriggerMachine::new();
        let g = mod_tap(&mut m, T);
        assert_eq!(
            g,
            Some(Gesture {
                kind: GestureKind::Word,
                undo: false
            })
        );
    }

    #[test]
    fn slow_mod_release_is_no_tap() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::MetaLeft, T), None);
        assert_eq!(release(&mut m, Key::MetaLeft, T + TAP_WINDOW_MS + 50), None);
    }

    #[test]
    fn key_between_mod_press_and_release_cancels_tap() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::MetaLeft, T), None);
        assert_eq!(press(&mut m, Key::Other, T + 50), None);
        assert_eq!(release(&mut m, Key::Other, T + 80), None);
        assert_eq!(release(&mut m, Key::MetaLeft, T + 100), None);
    }

    #[test]
    fn shift_press_with_mod_held_is_phrase_not_a_tap() {
        // Mod+Shift chord: the Shift press yields Phrase (converting on
        // Mod release); the Mod release itself taps nothing.
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::MetaLeft, T), None);
        assert_eq!(
            press(&mut m, Key::ShiftLeft, T + 50),
            Some(Gesture {
                kind: GestureKind::Phrase,
                undo: false
            })
        );
        assert_eq!(release(&mut m, Key::ShiftLeft, T + 80), None);
        assert_eq!(release(&mut m, Key::MetaLeft, T + 100), None);
    }

    #[test]
    fn shift_held_through_release_voids_mod_tap() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::ShiftLeft, T), None);
        assert_eq!(press(&mut m, Key::MetaLeft, T + 50), None);
        assert_eq!(release(&mut m, Key::MetaLeft, T + 100), None);
        assert_eq!(release(&mut m, Key::ShiftLeft, T + 150), None);
    }

    #[test]
    fn chord_of_both_metas_is_no_tap() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::MetaLeft, T), None);
        assert_eq!(press(&mut m, Key::MetaRight, T + 50), None);
        assert_eq!(release(&mut m, Key::MetaLeft, T + 100), None);
        assert_eq!(release(&mut m, Key::MetaRight, T + 150), None);
    }

    #[test]
    fn ctrl_plus_mod_tap_is_selection() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::CtrlLeft, T), None);
        assert_eq!(press(&mut m, Key::MetaLeft, T + 50), None);
        let g = release(&mut m, Key::MetaLeft, T + 150);
        assert_eq!(
            g,
            Some(Gesture {
                kind: GestureKind::Selection,
                undo: false
            })
        );
        assert_eq!(release(&mut m, Key::CtrlLeft, T + 200), None);
    }

    #[test]
    fn repeated_mod_tap_undoes() {
        let mut m = TriggerMachine::new();
        let first = mod_tap(&mut m, T);
        assert!(matches!(first, Some(Gesture { undo: false, .. })));
        let second = mod_tap(&mut m, T + 500);
        assert_eq!(
            second,
            Some(Gesture {
                kind: GestureKind::Word,
                undo: true
            })
        );
    }

    #[test]
    fn meta_press_disturbs_shift_pair_but_tap_still_fires() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::ShiftLeft, T), None);
        assert_eq!(release(&mut m, Key::ShiftLeft, T + 50), None);
        assert_eq!(press(&mut m, Key::MetaLeft, T + 100), None);
        // The pair is disturbed, but the clean tap converts the word.
        let g = release(&mut m, Key::MetaLeft, T + 200);
        assert!(matches!(
            g,
            Some(Gesture {
                kind: GestureKind::Word,
                undo: false
            })
        ));
    }

    #[test]
    fn tap_off_disables_mod_tap() {
        let mut m = TriggerMachine::new();
        m.set_tap_action(None);
        assert_eq!(press(&mut m, Key::MetaLeft, T), None);
        assert_eq!(release(&mut m, Key::MetaLeft, T + 100), None);
    }

    #[test]
    fn pair_base_remaps_plain_double_shift() {
        let mut m = TriggerMachine::new();
        m.set_pair_base(GestureKind::Selection);
        let g = double_shift(&mut m, T);
        assert_eq!(
            g,
            Some(Gesture {
                kind: GestureKind::Selection,
                undo: false
            })
        );
    }

    /// Mod+Shift chord (Mod first): Shift press with Mod held is Phrase,
    /// converting on Mod release via the pending slot.
    #[test]
    fn mod_shift_press_is_phrase() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::MetaLeft, T), None);
        assert_eq!(
            press(&mut m, Key::ShiftLeft, T + 50),
            Some(Gesture {
                kind: GestureKind::Phrase,
                undo: false
            })
        );
        // Releases complete nothing further; the Mod release is no tap.
        assert_eq!(release(&mut m, Key::ShiftLeft, T + 100), None);
        assert_eq!(release(&mut m, Key::MetaLeft, T + 150), None);
    }

    #[test]
    fn mod_shift_repeat_is_undo() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::MetaLeft, T), None);
        let first = press(&mut m, Key::ShiftLeft, T + 50);
        assert!(matches!(
            first,
            Some(Gesture {
                kind: GestureKind::Phrase,
                undo: false
            })
        ));
        assert_eq!(release(&mut m, Key::ShiftLeft, T + 100), None);
        assert_eq!(release(&mut m, Key::MetaLeft, T + 150), None);
        assert_eq!(press(&mut m, Key::MetaLeft, T + 200), None);
        let second = press(&mut m, Key::ShiftLeft, T + 250);
        assert!(matches!(
            second,
            Some(Gesture {
                kind: GestureKind::Phrase,
                undo: true
            })
        ));
    }

    #[test]
    fn ctrl_mod_shift_is_selection() {
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::CtrlLeft, T), None);
        assert_eq!(press(&mut m, Key::MetaLeft, T + 10), None);
        assert_eq!(
            press(&mut m, Key::ShiftLeft, T + 50),
            Some(Gesture {
                kind: GestureKind::Selection,
                undo: false
            })
        );
    }

    #[test]
    fn shift_then_mod_is_nothing() {
        // Wrong order: Shift pressed first starts a pair, Mod press+release
        // afterwards taps nothing (Shift disturbed the tap) and completes
        // no pair.
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::ShiftLeft, T), None);
        assert_eq!(press(&mut m, Key::MetaLeft, T + 50), None);
        assert_eq!(release(&mut m, Key::MetaLeft, T + 100), None);
        assert_eq!(release(&mut m, Key::ShiftLeft, T + 150), None);
    }

    #[test]
    fn mod_shift_abandons_a_pending_pair() {
        // A Shift tap started a pair; Mod+Shift consumes the next Shift
        // press as Phrase, and the following lone press starts fresh.
        let mut m = TriggerMachine::new();
        assert_eq!(press(&mut m, Key::ShiftLeft, T), None);
        assert_eq!(release(&mut m, Key::ShiftLeft, T + 50), None);
        assert_eq!(press(&mut m, Key::MetaLeft, T + 100), None);
        assert!(matches!(
            press(&mut m, Key::ShiftLeft, T + 150),
            Some(Gesture {
                kind: GestureKind::Phrase,
                ..
            })
        ));
        assert_eq!(release(&mut m, Key::ShiftLeft, T + 200), None);
        assert_eq!(release(&mut m, Key::MetaLeft, T + 250), None);
        assert_eq!(press(&mut m, Key::ShiftLeft, T + 500), None);
    }

    fn mod_meta(m: &mut TriggerMachine, at: u64) {
        assert_eq!(press(m, Key::MetaLeft, at), None);
    }

    #[test]
    fn bind_fires_with_its_modifiers_held() {
        let mut m = TriggerMachine::new();
        m.set_binds(vec![Bind {
            mods: ModSet {
                meta: true,
                ..Default::default()
            },
            scancode: 38,
            kind: GestureKind::Word,
        }]);
        // No modifiers: no bind.
        assert_eq!(m.bind(38, T), None);
        mod_meta(&mut m, T + 10);
        let g = m.bind(38, T + 20);
        assert_eq!(
            g,
            Some(Gesture {
                kind: GestureKind::Word,
                undo: false
            })
        );
        // Unconfigured scancode: no bind.
        assert_eq!(m.bind(30, T + 30), None);
        release(&mut m, Key::MetaLeft, T + 40);
    }

    #[test]
    fn bind_allows_extra_modifiers() {
        let mut m = TriggerMachine::new();
        m.set_binds(vec![Bind {
            mods: ModSet {
                meta: true,
                ..Default::default()
            },
            scancode: 38,
            kind: GestureKind::Word,
        }]);
        mod_meta(&mut m, T);
        assert_eq!(press(&mut m, Key::CtrlLeft, T + 10), None);
        assert!(m.bind(38, T + 20).is_some());
    }

    #[test]
    fn bind_with_shift_modifier() {
        let mut m = TriggerMachine::new();
        m.set_binds(vec![Bind {
            mods: ModSet {
                meta: true,
                shift: true,
                ..Default::default()
            },
            scancode: 38,
            kind: GestureKind::Phrase,
        }]);
        mod_meta(&mut m, T);
        // Shift missing: no bind.
        assert_eq!(m.bind(38, T + 10), None);
        // The Shift press itself is the Mod+Shift phrase chord; the bind
        // right after counts as its repeat (shared gesture chain).
        assert_eq!(
            press(&mut m, Key::ShiftLeft, T + 20),
            Some(Gesture {
                kind: GestureKind::Phrase,
                undo: false
            })
        );
        let g = m.bind(38, T + 30);
        assert_eq!(
            g,
            Some(Gesture {
                kind: GestureKind::Phrase,
                undo: true
            })
        );
    }

    #[test]
    fn repeated_bind_undoes() {
        let mut m = TriggerMachine::new();
        m.set_binds(vec![Bind {
            mods: ModSet {
                meta: true,
                ..Default::default()
            },
            scancode: 38,
            kind: GestureKind::Word,
        }]);
        mod_meta(&mut m, T);
        assert!(matches!(
            m.bind(38, T + 10),
            Some(Gesture { undo: false, .. })
        ));
        assert_eq!(
            m.bind(38, T + 500),
            Some(Gesture {
                kind: GestureKind::Word,
                undo: true
            })
        );
    }

    #[test]
    fn held_meta_blocks_modifiers_free() {
        let mut m = TriggerMachine::new();
        assert!(m.modifiers_free());
        press(&mut m, Key::MetaLeft, T);
        assert!(!m.modifiers_free());
        assert!(m.meta_held());
        release(&mut m, Key::MetaLeft, T + 10);
        assert!(m.modifiers_free());
        press(&mut m, Key::MetaRight, T + 20);
        assert!(!m.modifiers_free());
        release(&mut m, Key::MetaRight, T + 30);
        assert!(m.modifiers_free());
    }

    #[test]
    fn modifiers_free_tracks_shift_and_ctrl() {
        let mut m = TriggerMachine::new();
        assert!(m.modifiers_free());
        press(&mut m, Key::ShiftLeft, T);
        assert!(!m.modifiers_free());
        release(&mut m, Key::ShiftLeft, T + 10);
        assert!(m.modifiers_free());
        press(&mut m, Key::CtrlRight, T + 20);
        assert!(!m.modifiers_free());
        release(&mut m, Key::CtrlRight, T + 30);
        assert!(m.modifiers_free());
    }

    #[test]
    fn custom_double_shift_window_applies() {
        let mut m = TriggerMachine::new();
        m.set_timing(TimingConfig {
            double_shift_ms: 1000,
            ..Default::default()
        });
        assert_eq!(press(&mut m, Key::ShiftLeft, T), None);
        assert_eq!(release(&mut m, Key::ShiftLeft, T + 50), None);
        // Past the default 400 ms, inside the configured 1000 ms.
        let g = press(&mut m, Key::ShiftLeft, T + 600);
        assert!(matches!(
            g,
            Some(Gesture {
                kind: GestureKind::Word,
                ..
            })
        ));
    }

    #[test]
    fn custom_tap_window_applies() {
        let mut m = TriggerMachine::new();
        m.set_timing(TimingConfig {
            tap_ms: 50,
            ..Default::default()
        });
        assert_eq!(press(&mut m, Key::MetaLeft, T), None);
        assert_eq!(release(&mut m, Key::MetaLeft, T + 100), None);
    }

    #[test]
    fn pending_fires_only_when_free_before_the_deadline() {
        let gesture = Gesture {
            kind: GestureKind::Word,
            undo: false,
        };
        let pending = PendingGesture::new(gesture, T, PENDING_TIMEOUT_MS);
        assert!(!pending.ready(T, false));
        assert!(pending.ready(T, true));
        assert!(pending.ready(T + PENDING_TIMEOUT_MS, true));
        assert!(!pending.ready(T + PENDING_TIMEOUT_MS + 1, true));
    }

    #[test]
    fn pending_expires_past_the_deadline() {
        let gesture = Gesture {
            kind: GestureKind::Selection,
            undo: true,
        };
        let pending = PendingGesture::new(gesture, T, PENDING_TIMEOUT_MS);
        assert!(!pending.expired(T));
        assert!(!pending.expired(T + PENDING_TIMEOUT_MS));
        assert!(pending.expired(T + PENDING_TIMEOUT_MS + 1));
        assert_eq!(pending.kind, GestureKind::Selection);
        assert!(pending.undo);
    }
}
