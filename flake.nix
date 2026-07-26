{
  description = "Privacy-conscious file organization with declarative Nix integration";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    agent-skills-nix.url = "github:Kyure-A/agent-skills-nix";
    emil-skills = {
      url = "github:emilkowalski/skills";
      flake = false;
    };
  };

  outputs =
    inputs@{ self, nixpkgs, ... }:
    let
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsFor = system: import nixpkgs { inherit system; };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          temari = pkgs.callPackage ./nix/package.nix { };
        in
        {
          inherit temari;
          default = temari;
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = nixpkgs.lib.getExe self.packages.${system}.temari;
          meta.description = "Run the Temari CLI";
        };
      });

      overlays.default = final: _: {
        temari = final.callPackage ./nix/package.nix { };
      };

      homeManagerModules = {
        temari =
          { pkgs, ... }:
          {
            imports = [
              (import ./nix/home-manager.nix {
                temariPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.temari;
              })
            ];
          };
        default = self.homeManagerModules.temari;
      };

      checks = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          home-manager-module = pkgs.callPackage ./nix/tests/home-manager-module.nix {
            module = import ./nix/home-manager.nix { };
          };
          inherit (self.packages.${system}) temari;
        }
      );

      formatter = forAllSystems (system: (pkgsFor system).nixfmt-tree);

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
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
            packages =
              with pkgs;
              [
                cargo
                clippy
                nodejs
                openssl
                pkg-config
                pnpm
                rustc
                rustfmt
              ]
              ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux linuxPackages;

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
