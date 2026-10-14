{ lib, rustPlatform }:
let
  cargoToml = builtins.fromTOML (builtins.readFile ../../Cargo.toml);
in
rustPlatform.buildRustPackage {
  pname = "niri-punto";
  version = cargoToml.package.version;

  src = lib.fileset.toSource {
    root = ../..;
    fileset = lib.fileset.unions [
      ../../Cargo.toml
      ../../Cargo.lock
      ../../README.md
      ../../src
      ../../contrib
    ];
  };

  cargoLock = {
    lockFile = ../../Cargo.lock;
  };

  # Refresh after Cargo.lock changes: set to lib.fakeHash, build, paste the
  # expected hash from the error.
  cargoHash = "sha256-EaSIiDCAppX1uOL7PqDo7Iv236xyTxXm34Gc/pllp9Y=";

  postInstall = ''
    install -Dm644 contrib/70-niri-punto.rules $out/lib/udev/rules.d/70-niri-punto.rules
    install -Dm644 contrib/niri-punto.service $out/lib/systemd/user/niri-punto.service
    substituteInPlace $out/lib/systemd/user/niri-punto.service \
      --replace-fail '@@NIRI_PUNTO_EXE@@' "$out/bin/niri-punto"
  '';

  meta = {
    description = "Keyboard layout corrector for the niri Wayland compositor";
    homepage = "https://github.com/qam1s/niri-punto";
    license = lib.licenses.gpl3Plus;
    mainProgram = "niri-punto";
    platforms = lib.platforms.linux;
  };
}
