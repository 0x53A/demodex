# /// script
# dependencies = ["websockets>=15"]
# ///
"""Feature persistence and safe app-server restart with real Codex, no inference."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
from websockets.sync.client import unix_connect
from wormhole_client import call

ROOT = Path(__file__).resolve().parents[1]


def rpc(ws, method, params, ident=1):
    ws.send(json.dumps({"id": ident, "method": method, "params": params}))
    while True:
        message = json.loads(ws.recv(timeout=30))
        if message.get("id") == ident:
            assert "error" not in message, message
            return message["result"]


with tempfile.TemporaryDirectory(prefix="demodex-features-") as directory:
    root = Path(directory)
    workspace = root / "workspace"
    workspace.mkdir()
    profile = root / "profile"
    profile.mkdir()
    original = '[features]\nmulti_agent_v2 = false\nagent_message_board = false\n'
    (profile / "config.toml").write_text(original)
    data = root / "state"
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    binary = os.environ.get("DEMODEX_BIN", str(ROOT / "target/rust-pwa/debug/demodex"))
    with (root / "daemon.log").open("w+") as log:
        def launch():
            child = subprocess.Popen([binary, "--api-only", "--data-dir", str(data), "--bind", f"127.0.0.1:{port}", "--host-workspace", str(workspace), "--codex-home", str(profile)], stdout=log, stderr=log)
            for _ in range(200):
                assert child.poll() is None, "daemon exited"
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=.1):
                        return child
                except OSError:
                    time.sleep(.1)
            raise AssertionError("daemon did not start")

        child = launch()
        try:
            token = (data / "access-token").read_text().strip()
            def api(op):
                return call(f"http://127.0.0.1:{port}", token, op)
            def features():
                return api("Runtime")["features"]
            def enabled(name):
                return next(f["enabled"] for f in features()["catalog"]["data"] if f["name"] == name)
            def set_feature(name, value):
                return api({"SetRuntimeFeature": {"name": name, "enabled": value}})
            assert not enabled("multi_agent_v2")
            for name in ("multi_agent_v2", "agent_message_board"):
                set_feature(name, True)
            assert features()["restart_required"]
            assert not enabled("multi_agent_v2"), "Saving must not silently change running features"
            try:
                set_feature("not_a_codex_feature", True)
                raise AssertionError("unknown feature accepted")
            except AssertionError as error:
                assert "Unknown feature" in str(error), error
            session = api({"HostSession": {"input": {"name": "Feature fixture", "thread_id": None, "sandbox": "read-only", "cwd": str(workspace)}}})
            assert session["status"] == "connected", session
            old_target = session["targets"][0]["id"]
            # A thread owned by another client must block restart, even when idle.
            with unix_connect(str(data / "runtime/ipc/app.sock")) as ws:
                rpc(ws, "initialize", {"clientInfo": {"name": "fixture", "version": "1"}, "capabilities": {"experimentalApi": True}})
                ws.send(json.dumps({"method": "initialized", "params": {}}))
                other = rpc(ws, "thread/start", {"cwd": str(workspace)}, 2)["thread"]["id"]
                try:
                    api("RestartRuntime")
                    raise AssertionError("restart accepted an unmanaged thread")
                except AssertionError as error:
                    assert "outside the managed sessions" in str(error), error
                assert api("Runtime")["running"]
                rpc(ws, "thread/delete", {"threadId": other}, 3)
            api("RestartRuntime")
            assert enabled("multi_agent_v2") and enabled("agent_message_board")
            assert not features()["restart_required"]
            after = api({"Detail": {"id": session["id"]}})["session"]
            assert after["status"] == "disconnected" and after["thread_id"] == session["thread_id"], after
            new_session = api({"HostSession": {"input": {"name": "After restart", "thread_id": None, "sandbox": "read-only", "cwd": str(workspace)}}})
            assert new_session["targets"][0]["id"] != old_target
            assert (profile / "config.toml").read_text() == original
            child.terminate()
            child.wait(timeout=30)
            child = launch()
            assert enabled("multi_agent_v2"), "Overrides must survive daemon restart"
            for name in ("multi_agent_v2", "agent_message_board"):
                set_feature(name, None)
            api("RestartRuntime")
            assert not enabled("multi_agent_v2") and not enabled("agent_message_board")
            assert features()["saved"] == {}
            assert (profile / "config.toml").read_text() == original
            print("PASS: discovery, overrides, restart guard, disconnect, executor replacement, persistence and profile reset; no inference")
        except Exception:
            log.flush()
            log.seek(0)
            print(log.read()[-6000:])
            raise
        finally:
            child.terminate()
            child.wait(timeout=30)
