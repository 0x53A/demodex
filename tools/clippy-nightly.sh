#!/usr/bin/env bash
set -euo pipefail
export DEMODEX_RUST_COMPILER=clippy-driver
shift # Cargo passes the wrapped rustc executable first.
exec "$(dirname "$0")/rustc-nightly.sh" "$@"
