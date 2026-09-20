#!/usr/bin/env bash
set -euo pipefail
compiler="$(rustup which --toolchain nightly "${DEMODEX_RUST_COMPILER:-rustc}")"
toolchain="${compiler%/bin/*}"
extra=()
if [[ " $* " != *wasm32* ]]; then extra=(-C link-arg=-fuse-ld=bfd); fi
exec "${DEMODEX_DYNAMIC_LINKER:?Run inside shell.nix}" --library-path "$toolchain/lib:${DEMODEX_RUST_LIB_PATH}" "$compiler" "$@" "${extra[@]}"
