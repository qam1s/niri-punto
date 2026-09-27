# niri-punto

A Punto Switcher-style daemon for the niri Wayland compositor: text typed in
the wrong layout is converted on explicit user action, and the layout is
switched to match.

## Language

### Conversion

**Conversion**:
A manual rewrite of already-typed text into the other layout, triggered by an
explicit user action.
_Avoid_: fix, transliterate.

**Replay**:
Re-emitting previously recorded scancodes after the layout has switched, so
they render in the new layout.
_Avoid_: retype.

**Trigger**:
A Shift-key gesture (Double Shift and its Shift/Ctrl modifications) or a
lone Mod tap (tap for the word, Ctrl+tap for the selection) that starts a
conversion, or undoes the previous one when repeated.
_Avoid_: hotkey, shortcut.

**Buffer entry**:
One unit of remembered input: a (scancode, shift) pair.
_Avoid_: key event, keycode.

### Layouts

**Layout pair**:
The ordered pair of layout codes from the config; position in the pair maps to
the niri layout index.
_Avoid_: layout list.

**Layout index**:
A layout's position in niri's layout list; the only address ever used to
switch layouts.
_Avoid_: next, prev, toggle.

### Future

**Blacklist** (future, no behavior in 0.1.0):
The per-application list where automatic conversion stays off once auto mode
exists.
_Avoid_: ignore-list.
