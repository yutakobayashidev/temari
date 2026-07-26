{
  lib,
  module,
  runCommand,
  writeShellScriptBin,
}:

let
  package = writeShellScriptBin "temari" "exit 0";
  evaluated = lib.evalModules {
    modules = [
      {
        options = {
          assertions = lib.mkOption {
            type = lib.types.listOf lib.types.unspecified;
            default = [ ];
          };
          home.packages = lib.mkOption {
            type = lib.types.listOf lib.types.package;
            default = [ ];
          };
          systemd.user.services = lib.mkOption {
            type = lib.types.attrs;
            default = { };
          };
          systemd.user.timers = lib.mkOption {
            type = lib.types.attrs;
            default = { };
          };
        };
      }
      module
      {
        services.temari = {
          enable = true;
          inherit package;
          workspaces.downloads = {
            workspaceId = "workspace-1";
            configFile = "/home/alice/.config/temari/config.toml";
            stateFile = "/home/alice/.local/state/temari/managed.sqlite3";
            source = "/home/alice/Downloads";
            interval = "10m";
          };
        };
      }
    ];
  };
  assertionsPass = lib.all (assertion: assertion.assertion) evaluated.config.assertions;
  service = evaluated.config.systemd.user.services.temari-downloads;
  timer = evaluated.config.systemd.user.timers.temari-downloads;
in
assert assertionsPass;
assert service.Unit.ConditionPathIsDirectory == "/home/alice/Downloads";
assert service.Service.Type == "oneshot";
assert service.Service.UMask == "0077";
assert service.Service.EnvironmentFile == [ ];
assert lib.hasInfix "managed run workspace-1 --apply --yes" service.Service.ExecStart;
assert !(lib.hasInfix "/bin/sh" service.Service.ExecStart);
assert timer.Timer.OnBootSec == "2m";
assert timer.Timer.OnUnitActiveSec == "10m";
assert timer.Timer.Persistent;
assert timer.Install.WantedBy == [ "timers.target" ];
runCommand "temari-home-manager-module-test" { } "touch $out"
