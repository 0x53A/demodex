# /// script
# dependencies = ["playwright", "websockets>=15"]
# ///
"""Embedded score playback through real Yew/Wormhole; fake Codex, no inference.

Build with --apteronotus-pkg first. --dist selects a candidate static release.
"""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time

from playwright.sync_api import sync_playwright, expect
from websockets.sync.server import serve
from websockets.exceptions import ConnectionClosed
from wormhole_client import api

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--dist", type=Path, default=ROOT / "web/.rust-dist")
args = parser.parse_args()
assert (args.dist / "apteronotus.json").is_file(), "Bundle --apteronotus-pkg first"


def port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


class Codex:
    peer = None
    turns = 0

    def handle(self, peer):
        self.peer = peer
        try:
            for raw in peer:
                message = json.loads(raw)
                if "id" not in message:
                    continue
                method = message.get("method")
                result = {}
                if method == "config/read":
                    result = {"config": {}}
                elif method in ("thread/start", "thread/resume"):
                    result = {"thread": {"id": "music-thread", "turns": []}, "sandbox": {"type": "readOnly"}}
                elif method == "thread/read":
                    result = {"thread": {"id": "music-thread", "status": {"type": "idle"}}}
                elif method == "turn/start":
                    self.turns += 1
                peer.send(json.dumps({"id": message["id"], "result": result}))
        except ConnectionClosed:
            pass


with tempfile.TemporaryDirectory(prefix="demodex-music-") as temporary:
    directory = Path(temporary)
    daemon_port, web_port, codex_port = port(), port(), port()
    host, origin = f"http://127.0.0.1:{daemon_port}", f"http://127.0.0.1:{web_port}"
    codex = Codex()
    server = serve(codex.handle, "127.0.0.1", codex_port)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    processes = []
    with (directory / "daemon.log").open("w+") as log:
        try:
            binary = os.environ.get("DEMODEX_BIN", str(ROOT / "target/rust-pwa/debug/demodex"))
            for command in [
                ["--bind", f"127.0.0.1:{daemon_port}", "--data-dir", str(directory / "state"), "--api-only", "--allowed-origin", origin],
                ["web", "--bind", f"127.0.0.1:{web_port}", "--directory", str(args.dist.resolve())],
            ]:
                processes.append(subprocess.Popen([binary, *command], cwd=ROOT, stdout=log, stderr=log))
            for number in [daemon_port, web_port]:
                for attempt in range(200):
                    try:
                        with socket.create_connection(("127.0.0.1", number), timeout=.1):
                            break
                    except OSError:
                        time.sleep(.05)
                else:
                    raise AssertionError("server did not start")
            token = (directory / "state/access-token").read_text().strip()
            api(host, token, "/sessions", {"name": "Music fixture", "endpoint": f"ws://127.0.0.1:{codex_port}", "targets": []})
            with sync_playwright() as playwright:
                browser = playwright.chromium.launch(
                    executable_path=os.environ.get("DEMODEX_CHROME", "/run/current-system/sw/bin/google-chrome"),
                    args=["--no-sandbox", "--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader"],
                )
                page = browser.new_page(viewport={"width": 1200, "height": 900}, service_workers="block")
                errors = []
                page.on("pageerror", lambda error: errors.append(str(error)))
                page.on("console", lambda message: print("Browser:", message.text, flush=True) if message.type == "error" else None)
                page.add_init_script("""
                  window.audioContexts=[];
                  window.AudioContext=class extends AudioContext {
                    constructor(...args){super(...args);window.audioContexts.push(this);}
                  };
                """)
                page.goto(origin)
                page.get_by_role("button", name="+ connection", exact=True).click()
                page.get_by_label("Host URL").fill(host)
                page.get_by_label("Access token").fill(token)
                page.get_by_role("button", name="Save and connect").click()
                expect(page.locator("header .indicator")).to_have_text("connected to", timeout=20000)
                page.locator(".session").filter(has_text="Music fixture").click()
                page.locator(".session-error").get_by_role("button", name="Reconnect", exact=True).click()
                expect(page.locator(".session-heading .status")).to_have_text("connected")
                source = '-- 水\r\nlocal v = voice {graph = function() return sine(220) * 0.03 >> pan(0) end}\r\nplay(v, "c4 ~")\r\n'
                codex.peer.send(json.dumps({"method": "item/completed", "params": {"threadId": "music-thread", "item": {
                    "id": "score", "type": "agentMessage", "text": "```eod\r\n" + source + "```\r\n",
                }}}))
                opener = page.get_by_role("button", name="Open Apteronotus score 1")
                expect(opener).to_be_visible()
                assert page.locator("apteronotus-app").count() == 0
                assert not page.evaluate("window.audioContexts.length")
                # A failed load is visible and a later explicit open can retry.
                # Static SPA servers may return index.html with 200 for a
                # missing optional manifest. That must still be unavailable.
                page.route("**/apteronotus.json", lambda route: route.fulfill(status=200, content_type="text/html", body="<!doctype html><title>Demodex</title>"))
                opener.click()
                expect(page.locator(".score-player [role=alert]")).to_contain_text("does not include")
                page.get_by_role("button", name="Close player").click()
                page.unroute("**/apteronotus.json")
                opener.click()
                try:
                    expect(page.locator("apteronotus-app canvas")).to_be_visible(timeout=15000)
                except Exception:
                    print("Page errors:", errors)
                    print("Player DOM:", page.locator(".score-player").evaluate("e => e.outerHTML"))
                    print("Registered:", page.evaluate("!!customElements.get('apteronotus-app')"))
                    raise
                expect(page.locator(".score-player [role=status]")).to_have_count(0, timeout=60000)
                player = page.locator("apteronotus-app")
                assert player.get_attribute("source") == source
                assert not page.evaluate("window.audioContexts.length"), "opening must not start audio"
                canvas = player.locator("canvas")
                canvas.click(position={"x": 240, "y": 130})
                page.keyboard.press("Control+Enter")
                page.wait_for_function("window.audioContexts.some(c => c.state === 'running')")
                codex.peer.send(json.dumps({"method": "item/completed", "params": {"threadId": "music-thread", "item": {
                    "id": "score", "type": "agentMessage", "text": "```eod\n-- newer score\ninvalid(\n```\n",
                }}}))
                expect(page.locator('[data-item-id=score] > .rich-message')).to_contain_text("newer score")
                assert player.get_attribute("source") == source, "transcript updates must preserve the opened score snapshot"
                assert page.evaluate("window.audioContexts.some(c => c.state === 'running')")
                page.get_by_role("button", name="Close player").click()
                expect(player).to_have_count(0)
                page.wait_for_function("window.audioContexts.every(c => c.state !== 'running')")
                # Closing during asynchronous mount must leave no hidden editor.
                opener.click()
                page.get_by_role("button", name="Close player").click()
                expect(player).to_have_count(0)
                opener.click()
                expect(player.locator("canvas")).to_be_visible(timeout=60000)
                page.get_by_role("button", name="Close player").click()
                # A rejected document must not leave the starter-score editor
                # mounted behind the error, and a valid score can open afterward.
                oversized = "--" + "x" * (1024 * 1024)
                codex.peer.send(json.dumps({"method": "item/completed", "params": {"threadId": "music-thread", "item": {
                    "id": "score", "type": "agentMessage", "text": "```eod\n" + oversized + "\n```\n",
                }}}))
                expect(page.locator('[data-item-id=score] > .rich-message')).to_contain_text(oversized)
                opener.click()
                expect(page.locator(".score-player [role=alert]")).to_contain_text("document limit", timeout=60000)
                expect(page.locator(".score-player-host > *")).to_have_count(0)
                assert page.evaluate("window.audioContexts.every(c => c.state !== 'running')")
                page.get_by_role("button", name="Close player").click()
                codex.peer.send(json.dumps({"method": "item/completed", "params": {"threadId": "music-thread", "item": {
                    "id": "score", "type": "agentMessage", "text": "```eod\r\n" + source + "```\r\n",
                }}}))
                expect(page.locator('[data-item-id=score] > .rich-message')).to_contain_text("play(v,")
                opener.click()
                expect(player.locator("canvas")).to_be_visible(timeout=60000)
                expect(page.locator(".score-player [role=status]")).to_have_count(0, timeout=60000)
                assert player.get_attribute("source") == source
                page.get_by_role("button", name="Close player").click()
                assert codex.turns == 0, "playback must not send a model turn"
                assert not errors, errors
                browser.close()
            print("PASS: real embedded score, failed-load retry, exact source, explicit Run, close/reopen, rejected-document teardown and no inference")
        except Exception:
            log.flush()
            log.seek(0)
            print(log.read()[-6000:])
            raise
        finally:
            for process in reversed(processes):
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            server.shutdown()
