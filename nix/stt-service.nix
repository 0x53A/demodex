# Private Rust STT worker. Build the checkout with crates/stt/shell.nix first.
{ config, lib, pkgs, ... }:
let cfg = config.services.demodex-stt;
in {
  options.services.demodex-stt = {
    enable = lib.mkEnableOption "private speech transcription over a Unix socket";
    user = lib.mkOption { type = lib.types.str; description = "Existing SSH user allowed to use the private STT socket."; };
    binary = lib.mkOption { type = lib.types.path; description = "Built demodex-stt executable (checkout release path)."; };
    model = lib.mkOption { type = lib.types.path; description = "Locally provisioned Cohere GGUF model; never downloaded by the service."; };
    backend = lib.mkOption { type = lib.types.enum [ "cpu" "vulkan" "rocm" ]; default = "vulkan"; };
  };
  config = lib.mkIf cfg.enable {
    # Stable SSH command path; retain the user's checkout deployment convention.
    environment.systemPackages = [(pkgs.writeShellScriptBin "demodex-stt" ''
      exec ${lib.escapeShellArg (toString cfg.binary)} "$@"
    '')];
    systemd.services.demodex-stt = {
      description = "Private Cohere speech transcription";
      wantedBy = [ "multi-user.target" ];
      environment = {
        LD_LIBRARY_PATH = lib.makeLibraryPath [ pkgs.vulkan-loader pkgs.stdenv.cc.cc.lib ] + ":/run/opengl-driver/lib";
        HF_HUB_OFFLINE = "1";
      };
      serviceConfig = {
        User = cfg.user;
        SupplementaryGroups = [ "render" "video" ];
        ExecStart = lib.escapeShellArgs [ (toString cfg.binary) "serve" "--socket" "/run/demodex-stt/stt.sock" "--model" (toString cfg.model) "--backend" cfg.backend ];
        RuntimeDirectory = "demodex-stt";
        RuntimeDirectoryMode = "0700";
        UMask = "0077";
        Restart = "on-failure";
        RestartSec = 5;
        TimeoutStopSec = 15;
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = "read-only";
        RestrictAddressFamilies = [ "AF_UNIX" ];
        MemoryMax = "12G";
        TasksMax = 128;
      };
    };
  };
}
