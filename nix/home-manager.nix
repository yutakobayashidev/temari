{
  temariPackage ? null,
}:

{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.temari;

  workspaceModule = lib.types.submodule (
    { name, ... }:
    {
      options = {
        workspaceId = lib.mkOption {
          type = lib.types.strMatching "[A-Za-z0-9_-]+";
          default = name;
          description = "Managed workspace ID passed to Temari.";
        };

        configFile = lib.mkOption {
          type = lib.types.str;
          description = "Absolute path to the owner-only Temari configuration file.";
        };

        stateFile = lib.mkOption {
          type = lib.types.str;
          description = "Absolute path to the Temari managed-workspace state database.";
        };

        source = lib.mkOption {
          type = lib.types.str;
          description = "Absolute path to the managed source directory.";
        };

        interval = lib.mkOption {
          type = lib.types.str;
          default = "5m";
          description = "systemd OnUnitActiveSec value for finite managed runs.";
        };

        environmentFile = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = "Optional absolute systemd EnvironmentFile path for model credentials.";
        };
      };
    }
  );

  absolutePath = path: lib.hasPrefix "/" path;
  unitName = name: "temari-${name}";
  execStart =
    workspace:
    lib.escapeShellArgs [
      (lib.getExe cfg.package)
      "--config"
      workspace.configFile
      "--state"
      workspace.stateFile
      "--no-input"
      "managed"
      "run"
      workspace.workspaceId
      "--apply"
      "--yes"
    ];
in
{
  options.services.temari = {
    enable = lib.mkEnableOption "Temari and its declarative per-user schedules";

    package = lib.mkOption {
      type = lib.types.package;
      default = if temariPackage == null then pkgs.temari else temariPackage;
      defaultText = lib.literalExpression "pkgs.temari";
      description = "The Temari package to use.";
    };

    workspaces = lib.mkOption {
      type = lib.types.attrsOf workspaceModule;
      default = { };
      description = "Managed workspaces invoked by finite systemd user timers.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = lib.flatten (
      lib.mapAttrsToList (name: workspace: [
        {
          assertion = builtins.match "[A-Za-z0-9_-]+" name != null;
          message = "services.temari.workspaces.${name}: attribute name must contain only letters, digits, '_' or '-'";
        }
        {
          assertion = absolutePath workspace.configFile;
          message = "services.temari.workspaces.${name}.configFile must be an absolute path";
        }
        {
          assertion = absolutePath workspace.stateFile;
          message = "services.temari.workspaces.${name}.stateFile must be an absolute path";
        }
        {
          assertion = absolutePath workspace.source;
          message = "services.temari.workspaces.${name}.source must be an absolute path";
        }
        {
          assertion = workspace.environmentFile == null || absolutePath workspace.environmentFile;
          message = "services.temari.workspaces.${name}.environmentFile must be an absolute path";
        }
      ]) cfg.workspaces
    );

    home.packages = [ cfg.package ];

    systemd.user.services = lib.mapAttrs' (
      name: workspace:
      lib.nameValuePair (unitName name) {
        Unit = {
          Description = "Organize Temari workspace ${workspace.workspaceId}";
          ConditionPathIsDirectory = workspace.source;
        };
        Service = {
          Type = "oneshot";
          UMask = "0077";
          ExecStart = execStart workspace;
          EnvironmentFile = lib.optional (workspace.environmentFile != null) workspace.environmentFile;
        };
      }
    ) cfg.workspaces;

    systemd.user.timers = lib.mapAttrs' (
      name: workspace:
      lib.nameValuePair (unitName name) {
        Unit.Description = "Run Temari workspace ${workspace.workspaceId} periodically";
        Timer = {
          OnBootSec = "2m";
          OnUnitActiveSec = workspace.interval;
          Persistent = true;
        };
        Install.WantedBy = [ "timers.target" ];
      }
    ) cfg.workspaces;
  };
}
