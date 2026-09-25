# /// script
# dependencies = []
# ///
"""Real Codex prompt creation with a disposable profile; never runs inference."""
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
from wormhole_client import call

ROOT = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix="demodex-prompt-") as directory:
    root = Path(directory)
    workspace = root / "workspace"
    workspace.mkdir()
    (workspace / "AGENTS.md").write_text("THIS MUST NOT BE LOADED FOR AN OVERRIDE")
    data = root / "state"
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    binary = os.environ.get("DEMODEX_BIN", str(ROOT / "target/rust-pwa/debug/demodex"))
    with (root / "daemon.log").open("w+") as log:
        child = subprocess.Popen([binary, "--api-only", "--data-dir", str(data), "--bind", f"127.0.0.1:{port}", "--host-workspace", str(workspace)], stdout=log, stderr=log)
        try:
            for _ in range(200):
                assert child.poll() is None, "daemon exited"
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=.1):
                        break
                except OSError:
                    time.sleep(.1)
            token = (data / "access-token").read_text().strip()
            def api(op):
                return call(f"http://127.0.0.1:{port}", token, op)
            defaults = api("DefaultPrompt")
            assert defaults["model"] and len(defaults["text"]) > 1000, defaults
            session = api({"HostSessionWithPrompt":{"input":{"name":"Prompt fixture", "thread_id":None,"sandbox":"read-only","cwd":str(workspace)},"prompt":"Use exactly this custom base prompt."}})
            assert session["thread_id"] and session["status"] == "connected", session
            assert not session["error"], session
            assert not (data / "runtime/home/auth.json").exists()
            print("PASS: live catalogue default and custom Codex thread/start, no inference")
        except Exception:
            log.flush()
            log.seek(0)
            print(log.read()[-5000:])
            raise
        finally:
            child.terminate()
            child.wait(timeout=30)
