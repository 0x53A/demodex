# Import from a host's NixOS configuration, then configure services.demodex.
{ config, lib, pkgs, ... }:
let
  cfg = config.services.demodex;
in {
  options.services.demodex = {
    enable = lib.mkEnableOption "Demodex host session manager";
    user = lib.mkOption { type = lib.types.str; description = "Existing user that owns the Demodex host service."; };
    directory = lib.mkOption { type = lib.types.str; default = "${config.users.users.${cfg.user}.home}/demodex"; };
    bind = lib.mkOption { type = lib.types.str; default = "127.0.0.1:4780"; };
    codex = lib.mkOption { type = lib.types.package; };
    binary = lib.mkOption { type = lib.types.str; default = "${cfg.directory}/target/debug/demodex"; };
    apiOnly = lib.mkOption { type = lib.types.bool; default = false; };
    tailscaleUsers = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = "Exact Tailscale logins accepted on the private data-directory tailscale.sock proxy socket.";
    };
    allowedOrigins = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = "Exact HTTPS PWA origins allowed to connect to this daemon.";
    };
    web = {
      enable = lib.mkEnableOption "independent static PWA service";
      bind = lib.mkOption { type = lib.types.str; default = "127.0.0.1:4782"; };
      directory = lib.mkOption { type = lib.types.str; default = "${cfg.directory}/web/.rust-dist"; };
    };
    codexHome = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Existing Codex profile to reuse directly; null creates a dedicated profile.";
    };
  };
  config = lib.mkIf cfg.enable {
    systemd.services.demodex = {
      description = "Demodex host sessions and web console";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" "tailscaled.service" ];
      path = [ cfg.codex pkgs.bash pkgs.coreutils pkgs.nix pkgs.openssh ];
      environment = {
        HOME = config.users.users.${cfg.user}.home;
        SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
        PATH = lib.mkForce "${lib.makeBinPath [ cfg.codex pkgs.bash pkgs.coreutils pkgs.nix pkgs.openssh ]}:/run/wrappers/bin:/run/current-system/sw/bin";
      };
      serviceConfig = {
        User = cfg.user;
        WorkingDirectory = cfg.directory;
        ExecStart = lib.escapeShellArgs ([
          cfg.binary
          "--bind" cfg.bind
          "--data-dir" "${cfg.directory}/.demodex-host"
          "--web-dir" cfg.web.directory
          "--host-workspace" "${cfg.directory}/workspace"
        ] ++ lib.optionals (cfg.codexHome != null) [ "--codex-home" cfg.codexHome ]
          ++ lib.optionals cfg.apiOnly [ "--api-only" ]
          ++ lib.concatMap (user: [ "--tailscale-user" user ]) cfg.tailscaleUsers
          ++ lib.concatMap (origin: [ "--allowed-origin" origin ]) cfg.allowedOrigins);
        Restart = "on-failure";
        RestartSec = 5;
        TimeoutStopSec = 45;
        KillMode = "control-group";
        UMask = "0077";
      };
      # Deliberately no PrivateDevices, ProtectHome or NoNewPrivileges: this mode
      # grants the normal user's hardware access and existing sudo policy.
    };
    systemd.services.demodex-web = lib.mkIf cfg.web.enable {
      description = "Demodex static PWA";
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        User = cfg.user;
        ExecStart = lib.escapeShellArgs [ cfg.binary "web" "--bind" cfg.web.bind "--directory" cfg.web.directory ];
        Restart = "on-failure";
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
      };
    };
  };
}
