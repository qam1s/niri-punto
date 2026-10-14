{ config, lib, pkgs, ... }:
let
  cfg = config.services.niri-punto;
in
{
  options.services.niri-punto = {
    enable = lib.mkEnableOption "niri-punto, keyboard layout corrector for the niri Wayland compositor";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix { };
      description = "The niri-punto package to use.";
    };
  };

  config = lib.mkIf cfg.enable {
    # The uaccess rule needs the driver loaded, else the node is static and matches nothing.
    boot.kernelModules = [ "uinput" ];

    services.udev.packages = [ cfg.package ];

    # Puts `niri-punto doctor` on PATH; no setup step is needed.
    environment.systemPackages = [ cfg.package ];

    systemd.user.services.niri-punto = {
      description = "niri-punto layout correction daemon";
      partOf = [ "graphical-session.target" ];
      after = [ "graphical-session.target" ];
      wantedBy = [ "graphical-session.target" ];
      serviceConfig = {
        ExecStart = lib.getExe cfg.package;
        Restart = "on-failure";
      };
    };
  };
}
