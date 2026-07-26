{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "temari";
  version = "0.1.0";
  src = lib.cleanSource ../.;

  cargoLock.lockFile = ../Cargo.lock;
  cargoBuildFlags = [
    "-p"
    "temari-cli"
  ];
  cargoTestFlags = [ "--workspace" ];

  meta = {
    description = "Privacy-conscious AI-assisted file organizer";
    homepage = "https://github.com/yutakobayashidev/temari";
    license = lib.licenses.unfree;
    mainProgram = "temari";
    platforms = lib.platforms.unix;
  };
}
