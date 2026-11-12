self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.gmessages;
in
{
  options.services.gmessages = {
    enable = lib.mkEnableOption "the Google Messages backend for the Omarchy Messages plugin";
    package = lib.mkOption {
      type = lib.types.package;
      default = (self.overlays.default pkgs pkgs).gmessages-tui;
      defaultText = lib.literalExpression "gmessages.packages.\${system}.gmessages-tui";
      description = "The gmessages-tui package that provides `gmessages-tui serve`.";
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    systemd.user.services.gmessages-tui = {
      Unit = {
        Description = "Google Messages backend";
        PartOf = [ "graphical-session.target" ];
        After = [ "graphical-session.target" ];
      };
      Service = {
        ExecStart = "${cfg.package}/bin/gmessages-tui serve";
        RuntimeDirectory = "gmessages-tui";
        RuntimeDirectoryMode = "0700";
        Restart = "on-failure";
        RestartSec = 5;
        # 3: another gmessages-tui serve holds the lock; restarting would only loop.
        RestartPreventExitStatus = 3;
      };
      Install.WantedBy = [ "graphical-session.target" ];
    };
  };
}
