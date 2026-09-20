{ pkgs ? import <nixpkgs> {}, hostOnly ? false }:
pkgs.mkShell {
  packages = with pkgs; [ cargo rustc rustfmt clippy pkg-config uv openssh trunk rustup llvmPackages.lld ]
    ++ lib.optionals (!hostOnly) [ qemu virtiofsd ];
  DEMODEX_DYNAMIC_LINKER = pkgs.stdenv.cc.bintools.dynamicLinker;
  RUSTC = "${./tools/rustc-nightly.sh}";
  DEMODEX_RUST_LIB_PATH = pkgs.lib.makeLibraryPath [ pkgs.zlib pkgs.stdenv.cc.cc.lib pkgs.openssl ];
  CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER = "${pkgs.llvmPackages.lld}/bin/wasm-ld";
  CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "-C link-arg=-fuse-ld=bfd";
}
