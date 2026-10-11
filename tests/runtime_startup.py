"""Failed isolated runtime startup releases its owned process; no inference."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("DEMODEX_BIN", ROOT / "target/rust-pwa/debug/demodex"))

def live_identity(pid):
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    except FileNotFoundError:
        return None
    return fields[19] if fields[0] != "Z" else None


with tempfile.TemporaryDirectory(prefix="demodex-runtime-startup-") as temporary:
    root = Path(temporary)
    fake = root / "bin"
    fake.mkdir()
    # Stay alive without opening the expected app-server socket.
    bwrap = fake / "bwrap"
    bwrap.write_text('#!/bin/sh\nprintf "%s\\n" "$$" > "$DEMODEX_TEST_CHILD_PID"\nexec sleep 120\n')
    bwrap.chmod(0o700)
    pid_file = root / "child.pid"
    env = dict(os.environ, PATH=str(fake) + ":" + os.environ["PATH"],
               DEMODEX_TEST_CHILD_PID=str(pid_file))
    with socket.socket() as wire:
        wire.bind(("127.0.0.1", 0))
        port = wire.getsockname()[1]
    data = root / "state"
    with (root / "daemon.log").open("w") as log:
        daemon = subprocess.Popen([BIN, "--data-dir", data, "--bind",
                                   f"127.0.0.1:{port}", "--api-only"],
                                  env=env, stdout=log, stderr=log)
        caller = None
        try:
            for _ in range(200):
                assert daemon.poll() is None, (root / "daemon.log").read_text()
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=.1):
                        break
                except OSError:
                    time.sleep(.05)
            else:
                raise AssertionError("daemon did not become ready")
            # A second attempt checks that failure leaves the runtime retryable.
            for _ in range(2):
                pid_file.unlink(missing_ok=True)
                caller = subprocess.Popen([BIN, "call", "--url",
                                           f"ws://127.0.0.1:{port}/wormhole",
                                           "--token-file", data / "access-token"],
                                          stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                          stderr=subprocess.PIPE, text=True)
                caller.stdin.write(json.dumps({"operation": "StartRuntime"}) + "\n")
                caller.stdin.close()
                caller.stdin = None
                for _ in range(200):
                    if pid_file.exists() and pid_file.read_text().strip():
                        break
                    assert caller.poll() is None, "startup returned before spawning the fixture"
                    time.sleep(.025)
                else:
                    raise AssertionError("startup never launched bwrap")
                pid = int(pid_file.read_text())
                identity = live_identity(pid)
                assert identity is not None, "fixture exited before readiness failure"
                stdout, stderr = caller.communicate(timeout=30)
                assert caller.returncode == 0, stderr
                result = json.loads(stdout)
                assert "socket did not become ready" in result.get("Err", ""), result
                for _ in range(100):
                    if live_identity(pid) != identity:
                        break
                    time.sleep(.05)
                else:
                    raise AssertionError("failed startup leaked a live child")
                assert daemon.poll() is None, "startup failure stopped the daemon"
            print("Repeated failed runtime startup cleans up owned children (no inference).")
        except BaseException:
            log.flush()
            print((root / "daemon.log").read_text()[-6000:])
            raise
        finally:
            if caller is not None and caller.poll() is None:
                caller.terminate()
                caller.wait(timeout=15)
            if daemon.poll() is None:
                daemon.terminate()
            daemon.wait(timeout=30)
