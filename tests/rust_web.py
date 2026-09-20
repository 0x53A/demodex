# /// script
# dependencies = ["playwright", "websockets>=15"]
# ///
"""Real Yew/Wormhole browser integration with a fake Codex; no inference."""
import importlib.util
import json
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request

from playwright.sync_api import sync_playwright, expect
from websockets.sync.server import serve
from websockets.sync.client import connect
from websockets.exceptions import ConnectionClosed

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target/rust-pwa/debug/demodex"


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_port(number):
    for _ in range(200):
        try:
            with socket.create_connection(("127.0.0.1", number), timeout=.1):
                return
        except OSError:
            time.sleep(.05)
    raise AssertionError(f"port {number} did not start")


class Codex:
    def __init__(self):
        self.socket = None
        self.calls = []
        self.answers = []
        self.connections = 0

    def handle(self, ws):
        try:
            self.connected(ws)
        except ConnectionClosed:
            pass

    def connected(self, ws):
        self.socket = ws
        self.connections += 1
        for raw in ws:
            message = json.loads(raw)
            if "method" not in message:
                self.answers.append(message)
                self.send("serverRequest/resolved", {"requestId": message["id"]})
                self.send("turn/completed", {"turn": {"id": "turn"}})
                continue
            self.calls.append(message)
            if "id" not in message:
                continue
            method = message["method"]
            result = {}
            if method in ("thread/start", "thread/resume"):
                result = {"thread": {"id": "thread", "turns": []}, "sandbox": {"type": "readOnly"}}
            elif method == "turn/start":
                result = {"turn": {"id": "turn"}}
            ws.send(json.dumps({"id": message["id"], "result": result}))
            if method == "turn/start":
                self.send("turn/started", {"turn": {"id": "turn"}})
                for i in range(40):
                    self.send("item/completed", {"item": {"id": str(i), "type": "agentMessage", "text": "Fixture transcript line " + str(i)}})

    def send(self, method, params, ident=None):
        message = {"method": method, "params": {"threadId": "thread", **params}}
        if ident is not None:
            message["id"] = ident
        self.socket.send(json.dumps(message))


with tempfile.TemporaryDirectory(prefix="demodex-rust-web-") as temporary:
    directory = Path(temporary)
    candidate = directory / "web"
    shutil.copytree(ROOT / "web/.rust-dist", candidate)
    daemon_port, web_port, codex_port = port(), port(), port()
    origin = f"http://127.0.0.1:{web_port}"
    host = f"http://127.0.0.1:{daemon_port}"
    codex = Codex()
    server = serve(codex.handle, "127.0.0.1", codex_port)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    incompatible_port = port()
    incompatible_frames = []

    def incompatible_client(socket):
        socket.send(json.dumps({"protocol": "demodex", "version": 999, "schema": "old-schema"}))
        try:
            incompatible_frames.append(socket.recv(timeout=5))
        except (ConnectionClosed, TimeoutError):
            pass

    incompatible_server = serve(incompatible_client, "127.0.0.1", incompatible_port)
    threading.Thread(target=incompatible_server.serve_forever, daemon=True).start()
    processes = []
    log = (directory / "daemon.log").open("w+")

    def start(*args):
        process = subprocess.Popen([str(BINARY), *args], cwd=ROOT, stdout=log, stderr=log)
        processes.append(process)
        return process

    try:
        daemon = start("--bind", f"127.0.0.1:{daemon_port}", "--data-dir", str(directory / "state"), "--api-only", "--allowed-origin", origin)
        web = start("web", "--bind", f"127.0.0.1:{web_port}", "--directory", str(candidate))
        wait_port(daemon_port)
        wait_port(web_port)
        token = (directory / "state/access-token").read_text().strip()
        # Even native clients must pass compatibility before the first actor
        # handshake. Neither credentials nor any binary frames are sent here.
        for field, incompatible in [("version", 999), ("schema", "incompatible")]:
            with connect(host.replace("http:", "ws:") + "/wormhole") as peer:
                hello = json.loads(peer.recv(timeout=5))
                assert hello["protocol"] == "demodex" and len(hello["schema"]) == 64
                hello[field] = incompatible
                peer.send(json.dumps(hello))
                try:
                    peer.recv(timeout=5)
                    raise AssertionError("incompatible client entered actor transport")
                except ConnectionClosed:
                    pass

        def api(path):
            req = urllib.request.Request(host + "/api" + path, headers={"Authorization": "Bearer " + token})
            return json.load(urllib.request.urlopen(req))

        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(executable_path="/run/current-system/sw/bin/google-chrome", headless=True, args=["--no-sandbox"])
            context = browser.new_context(viewport={"width": 1200, "height": 850})
            context.add_init_script("window.fixtureSockets=[]; window.WebSocket=class extends WebSocket {constructor(...args){super(...args);window.fixtureSockets.push(this);}};")
            page = context.new_page()
            errors = []
            rest_requests = []
            page.on("pageerror", lambda error: errors.append(str(error)))
            page.on("request", lambda request: rest_requests.append(request.url) if "/api/" in request.url else None)
            page.goto(origin)
            page.set_viewport_size({"width": 390, "height": 844})
            page.get_by_label("Host URL").fill(f"http://127.0.0.1:{port()}")
            page.get_by_label("Access token").fill(token)
            page.get_by_role("button", name="Connect host").click()
            expect(page.get_by_role("alert")).to_contain_text("Nearby devices", timeout=20000)
            expect(page.get_by_role("alert")).to_be_in_viewport()
            expect(page.get_by_role("button", name="Retry connection")).to_be_visible()
            page.get_by_role("button", name="Retry connection").click()
            expect(page.get_by_role("alert")).to_be_visible()
            expect(page.locator("header .indicator")).to_have_text("DISCONNECTED", timeout=20000)
            page.get_by_label("Host URL").fill(f"http://127.0.0.1:{incompatible_port}")
            page.get_by_label("Access token").fill(token)
            page.get_by_role("button", name="Connect host").click()
            expect(page.get_by_role("alert")).to_contain_text("Protocol mismatch", timeout=20000)
            assert not incompatible_frames, "client sent data before checking compatibility"
            page.get_by_label("Host URL").fill(host)
            page.get_by_label("Access token").fill("wrong-token")
            page.get_by_role("button", name="Connect host").click()
            expect(page.get_by_role("alert")).to_contain_text("Access token rejected", timeout=20000)
            page.get_by_label("Access token").fill(token)
            page.get_by_role("button", name="Connect host").click()
            expect(page.locator("header .indicator")).to_have_text("CONNECTED", timeout=20000)
            expect(page.get_by_role("alert")).to_have_count(0)
            page.set_viewport_size({"width": 1200, "height": 850})
            page.get_by_role("button", name="+ External").click()
            page.get_by_label("Name", exact=True).fill("Wormhole fixture")
            page.get_by_label("App-server WebSocket").fill(f"ws://127.0.0.1:{codex_port}")
            page.get_by_role("button", name="Create session", exact=True).click()
            page.get_by_role("button", name="Connect / resume").click()
            expect(page.locator(".session-heading .status")).to_have_text("connected")
            page.get_by_label("Message", exact=True).fill("Begin fixture")
            page.get_by_role("button", name="Send", exact=True).click()
            expect(page.locator(".activity")).to_contain_text("Working")
            expect(page.get_by_role("button", name="Connect / resume")).to_have_count(0)
            expect(page.locator("article")).to_have_count(40)
            session_id = api("/sessions")[0]["id"]
            page.get_by_label("Message", exact=True).fill("unsent draft")
            codex.send("item/tool/requestUserInput", {"questions": [{"id": "choice", "question": "Which workspace?", "options": [{"label": "One", "description": "First workspace"}]}]}, 777)
            question = page.get_by_label("Which workspace?", exact=False)
            question.fill("Explicit answer")
            assert not codex.answers
            # Both viewport sizes retain the header/composer while the transcript scrolls.
            for width, height in [(1200, 850), (390, 844)]:
                page.set_viewport_size({"width": width, "height": height})
                frame = page.evaluate("""() => { const r=s=>document.querySelector(s).getBoundingClientRect(); return {header:r('header').top, composer:r('.composer').bottom, height:innerHeight, body:document.body.scrollHeight, transcript:document.querySelector('.transcript').scrollHeight}; }""")
                assert frame["header"] == 0 and frame["composer"] <= height + 1, frame
                assert frame["body"] <= height + 1 and frame["transcript"] > height, frame
            page.get_by_role("button", name="Connections", exact=True).click()
            expect(page.get_by_role("heading", name="Your connections")).to_be_visible()
            page.go_back()
            expect(page.get_by_label("Message", exact=True)).to_have_value("unsent draft")
            page.go_forward()
            expect(page.get_by_role("heading", name="Your connections")).to_be_visible()
            page.go_back()
            expect(page.get_by_label("Message", exact=True)).to_have_value("unsent draft")
            history_length = page.evaluate("history.length")
            assert token not in page.evaluate("JSON.stringify(history.state)")
            page.reload()
            expect(page.get_by_label("Message", exact=True)).to_have_value("unsent draft")
            assert page.evaluate("history.length") == history_length
            expect(page.get_by_label("Message", exact=True)).to_have_value("unsent draft", timeout=20000)
            expect(question).to_have_value("Explicit answer")
            assert codex.connections == 1 and not codex.answers
            assert sum(c.get("method") == "turn/start" for c in codex.calls) == 1
            # Close only the browser transport. The daemon's Codex socket and
            # pending question must survive automatic reconnect.
            page.evaluate("window.fixtureSockets.forEach(socket=>socket.close())")
            expect(page.locator("header .indicator")).to_have_text("DISCONNECTED", timeout=20000)
            page.evaluate("window.dispatchEvent(new Event('online'))")
            expect(page.locator("header .indicator")).to_have_text("CONNECTED", timeout=20000)
            expect(question).to_have_value("Explicit answer")
            assert codex.connections == 1 and not codex.answers
            # Publish another static release while the daemon keeps the same live RPC.
            page.wait_for_function("!!navigator.serviceWorker.controller")
            with (candidate / "pwa.js").open("a") as output:
                output.write("\n// second fixture release\n")
            spec = importlib.util.spec_from_file_location("build_web", ROOT / "tools/build-web.py")
            builder = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(builder)
            builder.seal_release(candidate)
            page.evaluate("async () => (await navigator.serviceWorker.getRegistration()).update()")
            expect(page.get_by_role("button", name="Update now")).to_be_visible(timeout=20000)
            assert not codex.answers and daemon.poll() is None
            page.get_by_role("button", name="Update now").click()
            expect(page.get_by_role("button", name="Update now")).to_have_count(0, timeout=20000)
            expect(question).to_have_value("Explicit answer", timeout=20000)
            expect(page.get_by_label("Message", exact=True)).to_have_value("unsent draft")
            assert codex.connections == 1 and not codex.answers
            page.get_by_role("button", name="Send answer", exact=True).click()
            expect(page.locator(".session-heading .status")).to_have_text("idle")
            assert codex.answers == [{"id": 777, "result": {"answers": {"choice": {"answers": ["Explicit answer"]}}}}], codex.answers
            assert api(f"/sessions/{session_id}")["session"]["status"] == "idle"
            second_port = port()
            second_host = f"http://127.0.0.1:{second_port}"
            start("--bind", f"127.0.0.1:{second_port}", "--data-dir", str(directory / "second"), "--api-only", "--allowed-origin", origin)
            wait_port(second_port)
            second_token = (directory / "second/access-token").read_text().strip()
            page.set_viewport_size({"width": 1200, "height": 850})
            page.locator("details.host-picker").evaluate("e=>e.open=true")
            page.get_by_label("Host URL").fill(second_host)
            expect(page.get_by_label("Access token")).to_have_value("")
            page.get_by_label("Access token").fill(second_token)
            page.get_by_role("button", name="Connect host").click()
            expect(page.locator("header .indicator")).to_have_text("CONNECTED")
            expect(page.locator("aside .session")).to_have_count(0)
            page.locator("details.host-picker").evaluate("e=>e.open=true")
            page.get_by_label("Host URL").fill(host)
            expect(page.get_by_label("Access token")).to_have_value(token)
            page.get_by_role("button", name="Connect host").click()
            page.get_by_role("button", name="Wormhole fixture").click()
            expect(page.get_by_label("Message", exact=True)).to_have_value("unsent draft")
            # A fresh PWA window has no sessionStorage, but retains named hosts
            # and their distinct tokens in localStorage.
            fresh = context.new_page()
            fresh.set_viewport_size({"width": 390, "height": 844})
            fresh.goto(origin)
            expect(fresh.get_by_role("heading", name="Your connections")).to_be_visible()
            expect(fresh.locator(".connection")).to_have_count(2)
            first = fresh.locator(".connection").filter(has=fresh.locator("small", has_text=host))
            first.get_by_role("button", name="Edit", exact=True).click()
            expect(fresh.get_by_label("Access token")).to_have_value(token)
            fresh.get_by_label("Connection name (optional)").fill("Laptop fixture")
            fresh.get_by_role("button", name="Connect host", exact=True).click()
            expect(fresh.locator("header .indicator")).to_have_text("CONNECTED")
            fresh.get_by_role("button", name="Connections", exact=True).click()
            expect(fresh.get_by_role("button", name="Laptop fixture", exact=False)).to_be_visible()
            second = fresh.locator(".connection").filter(has=fresh.locator("small", has_text=second_host))
            second.locator(".connection-open").click()
            expect(fresh.locator("header .indicator")).to_have_text("CONNECTED")
            expect(fresh.locator("aside .session")).to_have_count(0)
            fresh.go_back()
            expect(fresh.get_by_role("heading", name="Your connections")).to_be_visible()
            fresh.locator(".connection").filter(has=fresh.locator("small", has_text=second_host)).get_by_role("button", name="Forget").click()
            expect(fresh.locator(".connection")).to_have_count(1)
            assert second_token not in fresh.evaluate("localStorage.getItem('demodex-connections')")
            fresh.close()
            assert codex.connections == 1
            assert not rest_requests, rest_requests
            assert not errors, errors
            browser.close()
        print("PASS: Wormhole auth, commands/events, mobile frame, pending questions, reconnect, drafts, PWA update, switching independent hosts, no browser REST")
    except Exception:
        log.flush()
        log.seek(0)
        print(log.read()[-12000:])
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
        incompatible_server.shutdown()
