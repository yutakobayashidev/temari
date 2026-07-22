{
  description = "Temari development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    agent-skills-nix.url = "github:Kyure-A/agent-skills-nix";
    emil-skills = {
      url = "github:emilkowalski/skills";
      flake = false;
    };
  };

  outputs = inputs@{ self, nixpkgs, ... }:
    let
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "temari";
            version = "0.1.0";
            src = pkgs.lib.cleanSource ./.;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "-p" "temari-cli" ];
            cargoTestFlags = [ "--workspace" ];
          };
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/temari";
        };
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          linuxPackages = with pkgs; [
            atk
            cairo
            dbus
            gdk-pixbuf
            glib
            gtk3
            libsoup_3
            pango
            webkitgtk_4_1
          ];
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              clippy
              nodejs
              openssl
              pkg-config
              pnpm
              rustc
              rustfmt
            ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux linuxPackages;

            shellHook =
              let
                agentLib = inputs.agent-skills-nix.lib.agent-skills;
                sources = {
                  emil = {
                    path = inputs.emil-skills;
                    subdir = "skills";
                  };
                };
                catalog = agentLib.discoverCatalog sources;
                allowlist = agentLib.allowlistFor {
                  inherit catalog sources;
                  enableAll = true;
                };
                selection = agentLib.selectSkills {
                  inherit catalog allowlist sources;
                  skills = { };
                };
                bundle = agentLib.mkBundle { inherit pkgs selection; };
                localTargets = builtins.mapAttrs (
                  _: target:
                  target
                  // {
                    enable = true;
                  }
                ) agentLib.defaultLocalTargets;
              in
              agentLib.mkShellHook {
                inherit pkgs bundle;
                targets = localTargets;
              }
              + pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
                export XDG_DATA_DIRS="${pkgs.gtk3}/share/gsettings-schemas/${pkgs.gtk3.name}:''${XDG_DATA_DIRS:-}"
              '';
          };
        }
      );
    };
}
