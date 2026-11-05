{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  packages = with pkgs; [ cargo rustc cmake pkg-config clang vulkan-headers vulkan-loader glslang shaderc ];
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [ pkgs.vulkan-loader pkgs.stdenv.cc.cc.lib ];
}
