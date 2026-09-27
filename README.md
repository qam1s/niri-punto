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
[`wl-clipboard`](https://github.com/bugaevc/wl-clipboard).

1. Download `niri-punto-<version>-x86_64-unknown-linux-gnu.tar.gz` from the
   [Releases page](https://github.com/qam1s/niri-punto/releases) and verify
   the `.sha256` checksum.
2. Extract and run `./niri-punto setup`. Everything installs at user level
   (binary, systemd unit, default config); only the udev rule, the
   modules-load entry and the driver load need root — `setup` escalates
   just those steps (or pass `--no-udev` and install them by hand).

## Usage

| Gesture              | Action                            |
| -------------------- | --------------------------------- |
| Double Shift         | Convert last word                 |
| Mod tap              | Convert last word                 |
| Shift + Double Shift | Convert phrase                    |
| Ctrl + Double Shift  | Convert selection (via clipboard) |
| Ctrl + Mod tap       | Convert selection (via clipboard) |
| Repeat gesture       | Undo                              |

Prefer key binds? Add to your niri config:

```kdl
Mod+L { spawn "niri-punto" "convert-word"; }
```

Layouts are configured as an ordered pair in `config.kdl`; `niri-punto doctor`
checks devices, permissions, the niri socket, and the index mapping.

## Config

`setup` writes the default config to `$XDG_CONFIG_HOME/niri-punto/config.kdl`
and never overwrites an existing file. The core is the ordered layout pair:

```kdl
layouts "us" "ru"
```

Position in the pair maps to the niri layout index, so the order must match
the `layout` line in your niri config. `doctor` prints niri's layout names,
the current index, and the resulting correspondence. With more than two
system layouts, manual conversion works within the configured pair only.

## License

GPL-3.0-or-later [LICENSE](LICENSE).
