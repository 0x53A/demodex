#!/usr/bin/env -S uv run
"""Build the Rust PWA candidate without touching the deployed web/dist."""
import hashlib
import argparse
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
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--public-url", default="/", help="Absolute site path, e.g. /demodex/")
    parser.add_argument("--standalone", action="store_true", help="Start without assuming a same-origin daemon")
    parser.add_argument("--dist", type=Path, default=ROOT / "web/.rust-dist")
    args = parser.parse_args()
    if not args.public_url.startswith("/") or args.public_url.startswith("//") or any(c in args.public_url for c in "?#"):
        parser.error("--public-url must be an absolute site path")
    env = os.environ | {"CARGO_TARGET_DIR": str(ROOT / "target/rust-pwa"),
                        "DEMODEX_STANDALONE": "1" if args.standalone else "0", "NO_COLOR": "true"}
    # NixOS needs the loader wrapper; ordinary Linux runners use rustup directly.
    if env.get("DEMODEX_DYNAMIC_LINKER"):
        env["RUSTC"] = str(ROOT / "tools/rustc-nightly.sh")
    subprocess.run(["trunk", "build", "--release", "--locked", "--config", "crates/web/Trunk.toml",
                    "--public-url", args.public_url.rstrip("/") + "/", "--dist", str(args.dist.resolve())],
                   cwd=ROOT, env=env, check=True)
    print("PWA release:", seal_release(args.dist.resolve()))
