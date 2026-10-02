# niri-punto

<p align="center">
    <a href="https://github.com/qam1s/niri-punto/blob/main/LICENSE"><img alt="GitHub License" src="https://img.shields.io/github/license/qam1s/niri-punto?color=blue"></a>
    <a href="https://github.com/qam1s/niri-punto/releases"><img alt="GitHub Release" src="https://img.shields.io/github/v/release/qam1s/niri-punto?color=blue&logo=none"></a>
    <a href="https://github.com/qam1s/niri-punto/actions/workflows/build.yml"><img alt="Build" src="https://img.shields.io/github/actions/workflow/status/qam1s/niri-punto/build.yml?branch=main&logo=none"></a>
    <a href="https://github.com/qam1s/niri-punto/actions/workflows/coverage.yml"><img alt="Coverage" src="https://raw.githubusercontent.com/qam1s/niri-punto/gh-badges/badge.svg"></a>
</p>

Keyboard layout corrector for the [niri](https://github.com/niri-wm/niri)
Wayland compositor.

## How it works

The daemon reads key presses, remembers them as scancodes, and on your trigger
erases the typed text, switches the layout by index over niri IPC, and replays
the same scancodes, they render in the new layout. Repeating the gesture
undoes the conversion. It never grabs the keyboard: if the daemon dies, your
keyboard keeps working.

## Install

```sh
cargo install --git https://github.com/qam1s/niri-punto --locked && ~/.cargo/bin/niri-punto setup
```

Test a branch before it reaches main, then go back:

```sh
niri-punto update-test --branch <name>  # default branch: dev
niri-punto update  # back to main
```

## Usage

| Gesture        | Action | Description                                                  |
| -------------- | ------ | ------------------------------------------------------------ |
| Mod            | word   | Convert word or switch layout when empty (incl. after space) |
| Mod + Shift    | phrase | Convert phrase                                               |
| Double Shift   | phrase | Convert phrase                                               |
| Repeat gesture | undo   | Undo previous conversion                                     |

## Config

`~/.config/niri-punto/config.kdl` (`$XDG_CONFIG_HOME` respected when set).

```kdl
layout "us" "ru"

binds {
    Mod word
    Mod+Shift phrase
    Double-Shift phrase
}

timings {
    double-shift-ms 400
    undo-ms 3000
    debounce-ms 30
    pending-ms 2000
    tap-ms 300
}
```

## License

GPL-3.0-or-later [LICENSE](LICENSE).
