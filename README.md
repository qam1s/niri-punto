<h1 align="center">
  <img alt="niri-punto" src="assets/niri-punto.png">
</h1>

<p align="center">
    Keyboard layout corrector for the <a href="https://github.com/niri-wm/niri">niri</a> Wayland compositor.
</p>

<p align="center">
    <a href="https://github.com/qam1s/niri-punto/blob/main/LICENSE"><img alt="GitHub License" src="https://img.shields.io/github/license/qam1s/niri-punto?color=blue"></a>
    <a href="https://github.com/qam1s/niri-punto/actions/workflows/build.yml"><img alt="Build" src="https://img.shields.io/github/actions/workflow/status/qam1s/niri-punto/build.yml?branch=main&logo=none"></a>
    <a href="https://crates.io/crates/niri-punto"><img alt="Crates.io Downloads" src="https://img.shields.io/crates/d/niri-punto?color=red"></a>
    <a href="https://github.com/qam1s/niri-punto/actions/workflows/coverage.yml"><img alt="Coverage" src="https://img.shields.io/endpoint?url=https://raw.githubusercontent.com/qam1s/niri-punto/gh-badges/coverage.json"></a>
</p>

## How it works

The daemon reads key presses and stores them as scancodes. On your trigger
it erases the typed text. Then it switches the layout by index over niri IPC
and replays the same scancodes. They render in the new layout. Repeat the
gesture to undo the conversion. It never grabs the keyboard: if the daemon
dies, your keyboard keeps working.

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

| Gesture        | Action                                                  |
| -------------- | ------------------------------------------------------- |
| Mod            | Convert word or switch layout when empty (incl. after space) |
| Double Shift   | Convert phrase                                          |
| Repeat gesture | Undo previous conversion                                |

## License

GPL-3.0-or-later [LICENSE](LICENSE).
