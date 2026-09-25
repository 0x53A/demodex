{ pkgs, rustPlatform }:
rustPlatform.buildRustPackage {
  pname = "demodex";
  version = "0.1.0";
  src = pkgs.lib.fileset.toSource {
    root = ../.;
    fileset = pkgs.lib.fileset.unions [ ../Cargo.toml ../Cargo.lock ../src ../crates ];
  };
  cargoLock = {
    lockFile = ../Cargo.lock;
    allowBuiltinFetchGit = true;
  };
  cargoBuildFlags = [ "--package" "demodex" ];
  cargoTestFlags = [ "--package" "demodex" "--lib" "--bins" ];
  nativeBuildInputs = [ pkgs.cmake ];
}
