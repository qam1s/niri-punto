//! Bake an xkbcli-compiled keymap into the build when available.
//!
//! The selection path maps clipboard text char-by-char, so it needs the
//! symbol table for the configured layout pair. At build time we ask
//! `xkbcli compile-keymap` for the pair's layouts (default `us,ru`,
//! override with `NIRI_PUNTO_XKB_LAYOUTS`) and store the compiled keymap in
//! `OUT_DIR`, where `keymaps` parses group 1 vs group 2 into pairs. When
//! xkbcli is absent or fails, an empty file is baked instead and `keymaps`
//! falls back to its static US/RU table — the build never fails over this.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=NIRI_PUNTO_XKB_LAYOUTS");
    let layouts = env::var("NIRI_PUNTO_XKB_LAYOUTS").unwrap_or_else(|_| "us,ru".to_string());
    let baked = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by cargo"))
        .join("xkb_keymap.xkb");
    let text = Command::new("xkbcli")
        .args([
            "compile-keymap",
            "--layout",
            &layouts,
            "--option",
            "grp:alt_shift_toggle",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    if text.is_empty() {
        println!(
            "cargo:warning=xkbcli unavailable or failed for layouts {layouts}: baking an empty keymap, the static US/RU table applies"
        );
    }
    std::fs::write(&baked, text).expect("write the baked keymap");
}
