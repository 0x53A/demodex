#!/usr/bin/env -S uv run
"""Build the Rust PWA candidate without touching the deployed web/dist."""
import hashlib
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def seal_release(directory):
    template = (ROOT / "crates/web/service-worker.js").read_text()
    assets = [
        {"path": "./" + path.relative_to(directory).as_posix(),
         "hash": hashlib.sha256(path.read_bytes()).hexdigest()}
        for path in sorted(directory.rglob("*"))
        if path.is_file() and path.name != "service-worker.js" and path.suffix != ".map"
    ]
    version = hashlib.sha256((json.dumps(assets) + template).encode()).hexdigest()
    (directory / "service-worker.js").write_text(
        "const BUILD = " + json.dumps({"version": version, "assets": assets}) + ";\n" + template
    )
    return version


if __name__ == "__main__":
    env = os.environ | {"RUSTC": str(ROOT / "tools/rustc-nightly.sh"),
                        "CARGO_TARGET_DIR": str(ROOT / "target/rust-pwa"), "NO_COLOR": "true"}
    subprocess.run(["trunk", "build", "--release", "--config", "crates/web/Trunk.toml"],
                   cwd=ROOT, env=env, check=True)
    print("PWA release:", seal_release(ROOT / "web/.rust-dist"))
