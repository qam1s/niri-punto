# niri-punto

<p align="center">
    <a href="https://github.com/qam1s/niri-punto/blob/main/LICENSE"><img alt="GitHub License" src="https://img.shields.io/github/license/qam1s/niri-punto?color=blue"></a>
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
cargo install niri-punto && niri-punto setup
```

Or in your Nix flake:

```nix
{
  inputs.niri-punto.url = "github:qam1s/niri-punto";
  ...
  modules = [inputs.niri-punto.nixosModules.default];
}
```

## Usage

| Gesture        | Action | Description                                                  |
| -------------- | ------ | ------------------------------------------------------------ |
| Mod            | word   | Convert word or switch layout when empty (incl. after space) |
| Mod + Shift    | phrase | Convert phrase                                               |
| Double Shift   | phrase | Convert phrase                                               |
| Repeat gesture | undo   | Undo previous conversion                                     |

## License

GPL-3.0-or-later [LICENSE](LICENSE).
