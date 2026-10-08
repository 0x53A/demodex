# /// script
# dependencies = ["websockets>=15", "playwright"]
# ///
"""Real Codex CLI daemon takeover and browser warning; disposable profiles, no inference."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
from websockets.sync.client import unix_connect
from playwright.sync_api import sync_playwright, expect
from wormhole_client import call

ROOT = Path(__file__).resolve().parents[1]


def rpc(ws, method, params, ident=1):
    ws.send(json.dumps({"id": ident, "method": method, "params": params}))
    while True:
        reply = json.loads(ws.recv(timeout=30))
        if reply.get("id") == ident:
            assert "error" not in reply, reply
            return reply["result"]


def open_cli(profile):
    ws = unix_connect(str(profile / "app-server-control/app-server-control.sock"))
    rpc(ws, "initialize", {"clientInfo": {"name": "takeover_fixture", "version": "1"}, "capabilities": {"experimentalApi": True}})
    ws.send(json.dumps({"method": "initialized", "params": {}}))
    return ws


with tempfile.TemporaryDirectory(prefix="demodex-takeover-") as directory:
    root = Path(directory)
    profile, workspace, data = root / "profile", root / "workspace", root / "state"
    profile.mkdir()
    workspace.mkdir()
    original = 'model = "gpt-6.1-sol"\n[analytics]\nenabled = false\n'
    (profile / "config.toml").write_text(original)
    env = {**os.environ, "CODEX_HOME": str(profile)}
    binary = os.environ.get("DEMODEX_BIN", str(ROOT / "target/rust-pwa/debug/demodex"))
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    child, cli = None, None
    with (root / "demodex.log").open("w+") as log:
        def daemon(action):
            result = subprocess.run(["codex", "app-server", "daemon", action], env=env, capture_output=True, text=True, timeout=40)
            assert result.returncode == 0, result.stderr

        def new_thread(ws):
            thread = rpc(ws, "thread/start", {"cwd": str(workspace), "sandbox": "read-only"})["thread"]["id"]
            # Persist a disposable history item without invoking a model. Empty
            # new threads aren't necessarily discoverable by another app-server.
            rpc(ws, "thread/inject_items", {"threadId": thread, "items": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Takeover fixture history; no inference."}]}]})
            return thread

        try:
            daemon("start")
            cli = open_cli(profile)
            thread, other = new_thread(cli), new_thread(cli)
            child = subprocess.Popen([binary, "--data-dir", str(data), "--bind", f"127.0.0.1:{port}",
                "--web-dir", os.environ.get("DEMODEX_WEB_DIST", str(ROOT / "web/.rust-dist")),
                "--host-workspace", str(workspace), "--codex-home", str(profile)], stdout=log, stderr=log)
            for _ in range(300):
                assert child.poll() is None, "Demodex exited"
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=.1):
                        break
                except OSError:
                    time.sleep(.1)
            else:
                raise AssertionError("Demodex did not start")
            token = (data / "access-token").read_text().strip()
            def api(op, receipt=None):
                return call(f"http://127.0.0.1:{port}", token, op, receipt)
            session = api({"HostSession": {"input": {"name": "Takeover fixture", "thread_id": thread, "sandbox": "read-only", "cwd": str(workspace)}}})
            sid = session["id"]
            assert session["status"] == "disconnected" and "already has an active writer" in session["error"], session
            def detail():
                return api({"Detail": {"id": sid}})
            def command(view):
                return {"Takeover": {"id": sid, "expected_daemon": view["daemon"], "expected_threads": view["threads"]}}
            view = detail()["controls"]["takeover"]
            assert view["threads"] == sorted([thread, other]), view
            # A readable record is insufficient: both the kernel process identity
            # and the peer of the actual inspection connection must match.
            pid_file = profile / "app-server-daemon/daemon.pid"
            original_record = pid_file.read_bytes()
            for mismatch, error_text in [("start", "process identity changed"), ("peer", "another process")]:
                invalid = json.loads(original_record)
                if mismatch == "start":
                    invalid["processIdentity"]["startTicks"] += 1
                else:
                    invalid["pid"] = os.getpid()
                try:
                    pid_file.write_text(json.dumps(invalid))
                    rejected = detail()["controls"]["takeover"]
                    assert "daemon" not in rejected and error_text in rejected["error"], rejected
                    try:
                        api(command(view))
                        raise AssertionError("invalid identity takeover was accepted")
                    except AssertionError as error:
                        assert error_text in str(error), error
                    assert thread in rpc(cli, "thread/loaded/list", {})["data"]
                finally:
                    pid_file.write_bytes(original_record)
            bad = command(view)
            bad["Takeover"]["expected_daemon"] = "stale"
            for op in [bad]:
                try:
                    api(op)
                    raise AssertionError("stale takeover was accepted")
                except AssertionError as error:
                    assert "changed" in str(error), error
            third = new_thread(cli)
            try:
                api(command(view))
                raise AssertionError("changed thread list was accepted")
            except AssertionError as error:
                assert "changed" in str(error), error
            assert thread in rpc(cli, "thread/loaded/list", {})["data"]
            view = detail()["controls"]["takeover"]
            assert view["threads"] == sorted([thread, other, third]), view
            # Close the CLI connection: daemon retains the writer, just like closing a terminal.
            cli.close()
            cli = None
            assert detail()["controls"]["takeover"]["threads"] == view["threads"]
            with sync_playwright() as playwright:
                browser = playwright.chromium.launch(executable_path=os.environ.get("CHROME", "/run/current-system/sw/bin/google-chrome"), headless=True, args=["--no-sandbox"])
                page = browser.new_page(viewport={"width": 390, "height": 844})
                page.goto(f"http://127.0.0.1:{port}")
                page.get_by_role("button", name="+ connection", exact=True).click()
                page.get_by_label("Access token").fill(token)
                page.get_by_role("button", name="Save and connect", exact=True).click()
                expect(page.locator("header .indicator")).to_have_text("connected to", timeout=20000)
                page.locator(".session").filter(has_text="Takeover fixture").first.click()
                notice = page.get_by_role("region", name="Codex takeover")
                expect(notice).to_contain_text("interrupts all 3 loaded sessions", timeout=20000)
                expect(notice.get_by_role("button", name="Take over — stop Codex CLI server")).to_be_enabled()
                notice.get_by_text("Sessions affected", exact=True).click()
                for affected in view["threads"]:
                    expect(notice.get_by_text(affected, exact=True)).to_be_visible()
                assert page.evaluate("document.documentElement.scrollWidth <= window.innerWidth"), "takeover overflows mobile viewport"
                # Click the real action, then recover its durable request ID for a replay check.
                operation = command(view)
                notice.get_by_role("button", name="Take over — stop Codex CLI server").click()
                expect(notice).to_have_count(0, timeout=30000)
                saved = page.evaluate("JSON.parse(sessionStorage.getItem('demodex-rust-view'))")
                request = saved["receipts"][saved["host"]]
                result = api(operation, request)
                assert result == {"ok": True}, result
                after = detail()
                assert after["session"]["status"] in ("connected", "idle"), after["session"]
                assert after["session"]["thread_id"] == thread
                assert after["controls"]["takeover"] is None
                expect(notice).to_have_count(0, timeout=20000)
                browser.close()
            daemon("start")
            cli = open_cli(profile)
            surviving = new_thread(cli)
            assert api(operation, request) == result
            assert surviving in rpc(cli, "thread/loaded/list", {})["data"], "receipt replay stopped the replacement daemon"
            # A new request on a connected session is rejected before stop.
            try:
                api(operation)
                raise AssertionError("connected session takeover was accepted")
            except AssertionError as error:
                assert "no active-writer conflict" in str(error), error
            assert surviving in rpc(cli, "thread/loaded/list", {})["data"]
            assert (profile / "config.toml").read_text() == original
            events = api({"Events": {"id": sid, "after": 0}})
            assert "turn/started" not in json.dumps(events), "takeover started inference"
            print("PASS: real active-writer conflict, retained daemon after CLI close, browser scope warning, kernel identity/peer checks, stale identity/list guards, pinned stop/resume, receipt deduplication, no turn, profile preserved")
        except Exception:
            log.flush()
            log.seek(0)
            print(log.read()[-8000:])
            raise
        finally:
            if cli:
                cli.close()
            if child:
                child.terminate()
                child.wait(timeout=30)
            daemon("stop")
