# /// script
# dependencies = ["playwright", "websockets>=15"]
# ///
"""Compatibility entry point for the real Rust/Wormhole browser suite."""
from pathlib import Path
import runpy

runpy.run_path(str(Path(__file__).with_name("rust_web.py")), run_name="__main__")
