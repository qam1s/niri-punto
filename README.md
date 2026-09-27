# niri-punto

Keyboard layout corrector for the [niri](https://github.com/niri-wm/niri)
Wayland compositor. Typed text in the wrong layout? Hit the trigger and the text
is rewritten in the right layout, and the layout switches to match.

## How it works

The daemon reads key presses, remembers them as scancodes, and on your trigger
erases the typed text, switches the layout by index over niri IPC, and replays
the same scancodes, they render in the new layout. Repeating the gesture
undoes the conversion. It never grabs the keyboard: if the daemon dies, your
keyboard keeps working.

`niri-punto doctor` prints niri's layout names, the current index, and the resulting
correspondence. With more than two system layouts, manual conversion
works within the configured pair only. Each `binds` entry is the required
modifiers (`Mod`, `Shift`, `Ctrl`, at least one) held plus the key press;
key names are letters and digits. The bind key is the trigger, not text,
so the converted scope stays exact. A bad file stops the daemon at start,
like a bad `layout` node.

## Install

Requirements: [niri](https://github.com/niri-wm/niri),
[wl-clipboard](https://github.com/bugaevc/wl-clipboard).

1. Download `niri-punto-<version>-x86_64-unknown-linux-gnu.tar.gz` from the
   [Releases page](https://github.com/qam1s/niri-punto/releases) and verify
   the `.sha256` checksum.
2. Extract and run `./niri-punto setup`. Everything installs at user level
   (binary, systemd unit, default config); only the udev rule, the
   modules-load entry and the driver load need root, `setup` escalates
   just those steps (or pass `--no-udev` and install them by hand).

Or via cargo (needs a Rust toolchain):

```sh
cargo install --git https://github.com/qam1s/niri-punto --locked
niri-punto setup
```

## Usage

| Gesture        | Action    | Description                              |
| -------------- | --------- | ---------------------------------------- |
| Mod            | word      | Convert word or switch layout when empty |
| Mod + L        | phrase    | Convert phrase                           |
| Double Shift   | selection | Convert selection                        |
| Repeat gesture | undo      | Undo previous conversion                 |

## Config

`setup` writes the default config to `~/.config/niri-punto/config.kdl`
(`$XDG_CONFIG_HOME` respected when set) and never overwrites an existing
file.

```kdl
layout "us" "ru"

binds {
    Mod word
    Mod+L phrase
    Double-Shift selection
}

timings {
    double-shift-ms 400
    undo-ms 3000
    debounce-ms 30
    pending-ms 2000
    tap-ms 300
}
```

## Troubleshooting

Layout switched between typing and triggering (you typed `ghbdtn` in us,
something flipped to ru before you hit the trigger): the daemon reads the
typed text itself, not the current layout indicator. When it is confident
about the intended layout it converts toward that layout, ignoring which
one is current. When it is unsure it keeps the old behavior and toggles
from the current layout. Unsure inputs are short text (under 3 letters),
mixed-language phrases, transliteration (`privet`), and code-like tokens
(`http`, `cfg`): those always fall back to the current-layout toggle.

## Development

`just test` runs the test suite, `just lint` runs clippy and fmt checks.
Install the pre-commit hooks once per clone with `prek install`.
CI additionally runs `typos` and `cargo deny check advisories licenses`.

## License

GPL-3.0-or-later [LICENSE](LICENSE).
