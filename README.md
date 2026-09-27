# niri-punto

Punto Switcher-style layout correction for the [niri](https://github.com/niri-wm/niri)
Wayland compositor. Typed text in the wrong layout? Hit the trigger — the text
is rewritten in the right layout and the layout switches to match.

## How it works

The daemon reads key presses, remembers them as scancodes, and on your trigger
erases the typed text, switches the layout by index over niri IPC, and replays
the same scancodes — they render in the new layout. Repeating the gesture
undoes the conversion. It never grabs the keyboard: if the daemon dies, your
keyboard keeps working.

## Install

Requirements: [niri](https://github.com/niri-wm/niri),
[wl-clipboard](https://github.com/bugaevc/wl-clipboard).

1. Download `niri-punto-<version>-x86_64-unknown-linux-gnu.tar.gz` from the
   [Releases page](https://github.com/qam1s/niri-punto/releases) and verify
   the `.sha256` checksum.
2. Extract and run `./niri-punto setup`. Everything installs at user level
   (binary, systemd unit, default config); only the udev rule, the
   modules-load entry and the driver load need root — `setup` escalates
   just those steps (or pass `--no-udev` and install them by hand).

Or via cargo (needs a Rust toolchain):

```sh
cargo install --git https://github.com/qam1s/niri-punto --locked
niri-punto setup
```

## Usage

| Gesture        | Action                                   |
| -------------- | ---------------------------------------- |
| Mod            | Convert word or switch layout when empty |
| Mod + Shift    | Convert phrase                           |
| Double Shift   | Convert selection                        |
| Repeat gesture | Undo                                     |

## Config

`setup` writes the default config to `~/.config/niri-punto/config.kdl`
(`$XDG_CONFIG_HOME` respected when set) and never overwrites an existing
file. The full default, line by line:

```kdl
// Ordered layout pair: position maps to the niri layout index, so the
// order must match the `layout` line in your niri config.
layouts "us" "ru"
// Lone Mod tap trigger: `meta` converts on tap, `off` disables it.
tap "meta"
// Daemon-side binds in niri style, one per scope from the table above:
// Mod held + key press converts, with no niri binds needed.
binds {
    Mod+L word
    Mod+P phrase
    Mod+S selection
}
// Trigger timings in milliseconds: absent keys mean these defaults.
timings {
    double-shift-ms 400
    undo-ms 3000
    debounce-ms 30
    pending-ms 2000
    tap-ms 300
}
```

`doctor` prints niri's layout names, the current index, and the resulting
correspondence. With more than two system layouts, manual conversion
works within the configured pair only. Each `binds` entry is the required
modifiers (`Mod`, `Shift`, `Ctrl` — at least one) held plus the key press;
key names are letters and digits. The bind key is the trigger, not text,
so the converted scope stays exact. A bad file stops the daemon at start,
like a bad `layouts` node.

## License

GPL-3.0-or-later [LICENSE](LICENSE).
