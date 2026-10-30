# /// script
# dependencies = ["playwright", "websockets>=15"]
# ///
"""Context window persistence, idle reconnect and checkbox/slider; no inference."""
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time
import uuid
from playwright.sync_api import sync_playwright, expect
from websockets.sync.client import unix_connect
from wormhole_client import call

ROOT = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix="demodex-context-") as directory:
    root = Path(directory)
    workspace = root / "workspace"
    workspace.mkdir()
    profile = root / "profile"
    profile.mkdir()
    catalog = json.loads(subprocess.check_output(
        ["codex", "debug", "models", "--bundled"], env={**os.environ, "CODEX_HOME": str(profile)}))
    model = catalog["models"][0]["slug"]
    for row in catalog["models"]:
        row["context_window"] = 272000
        row["max_context_window"] = 872000
    smaller = next(row for row in catalog["models"] if row["slug"] != model and row.get("visibility") == "list")
    smaller["context_window"] = smaller["max_context_window"] = 128123
    catalogue_path = profile / "catalog.json"
    catalogue_path.write_text(json.dumps(catalog))
    cached = json.loads(json.dumps(catalog))
    for record in cached["models"]:
        record["max_context_window"] = 272000
    (profile / "models_cache.json").write_text(json.dumps(cached))
    original = f'model = "{model}"\nmodel_catalog_json = "{catalogue_path}"\n'
    (profile / "config.toml").write_text(original)
    data = root / "state"
    binary = os.environ.get("DEMODEX_BIN", str(ROOT / "target/rust-pwa/debug/demodex"))
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    with (root / "daemon.log").open("w+") as log:
        child = subprocess.Popen([binary, "--data-dir", str(data), "--bind", f"127.0.0.1:{port}",
            "--host-workspace", str(workspace), "--codex-home", str(profile),
            "--web-dir", os.environ.get("DEMODEX_WEB", str(ROOT / "web/.context-window-dist"))],
            stdout=log, stderr=log, start_new_session=True)
        try:
            for _ in range(300):
                assert child.poll() is None, "daemon exited"
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=.1): break
                except OSError: time.sleep(.1)
            token = (data / "access-token").read_text().strip()
            def api(op, receipt=None):
                return call(f"http://127.0.0.1:{port}", token, op, receipt)
            def reject(op, message):
                try: api(op)
                except AssertionError as error:
                    assert message in str(error), str(error)
                else: raise AssertionError("invalid operation succeeded")
            models = api("RuntimeModels")
            row = next(m for m in models["data"] if m["model"] == model)
            assert row["context_window"] == {"default":272000,"min":16000,"max":872000}, models
            base = {"name":"Context fixture", "targets":[{"id":"host","cwd":str(workspace)}], "sandbox":"read-only"}
            before = len(api("Sessions"))
            reject({"CreateSession":{"input":{**base,"context_window":1000000}}}, "between")
            assert len(api("Sessions")) == before
            session = api({"CreateSession":{"input":{**base,"context_window":500000}}})
            assert session["status"] == "connected", session
            sid = session["id"]
            def setting(): return api({"Detail":{"id":sid}})["controls"]["context_window"]
            assert setting() == {"window":500000,"resolved":500000}
            reject({"SetContextWindow":{"id":sid,"window":1000000}}, "between")
            reject({"SetContextWindow":{"id":sid,"window":600000}}, "no saved history")
            assert api({"Detail":{"id":sid}})["session"]["status"] == "connected"
            assert setting()["window"] == 500000
            # Materialize disposable history without starting an inference turn.
            with unix_connect(str(data / "runtime/ipc/app.sock")) as ws:
                def rpc(method, params, ident):
                    ws.send(json.dumps({"id":ident,"method":method,"params":params}))
                    while True:
                        reply = json.loads(ws.recv(timeout=30))
                        if reply.get("id") == ident:
                            assert "error" not in reply, reply
                            return reply["result"]
                rpc("initialize", {"clientInfo":{"name":"context-fixture","version":"1"},"capabilities":{"experimentalApi":True}}, 1)
                ws.send(json.dumps({"method":"initialized","params":{}}))
                rpc("thread/inject_items", {"threadId":session["thread_id"],"items":[{"type":"message","role":"user","content":[{"type":"input_text","text":"Disposable saved history; no inference."}]}]}, 2)
            request = {"SetContextWindow":{"id":sid,"window":600000}}
            receipt = str(uuid.uuid4())
            assert api(request, receipt) == {"window":600000,"resolved":600000}
            assert api(request, receipt) == {"window":600000,"resolved":600000}
            other = next(m for m in models["data"] if m["model"] == smaller["slug"])
            def choose(record):
                return {"Model":{"id":sid,"input":{"model":record["model"],"effort":record["defaultReasoningEffort"],"serviceTier":None}}}
            reject(choose(other), "between")
            assert api({"Detail":{"id":sid}})["controls"]["settings"]["effective"]["model"] == model
            assert api({"SetContextWindow":{"id":sid,"window":None}}) == {"window":None,"resolved":272000}
            api(choose(other))
            assert setting() == {"window":None,"resolved":128123}
            api(choose(row))
            assert setting() == {"window":None,"resolved":272000}
            (profile / "config.toml").write_text(original + "model_context_window = 192000\n")
            assert api({"SetContextWindow":{"id":sid,"window":None}}) == {"window":None,"resolved":192000}
            (profile / "config.toml").write_text(original)
            assert api({"SetContextWindow":{"id":sid,"window":None}}) == {"window":None,"resolved":272000}
            assert api({"Detail":{"id":sid}})["session"]["thread_id"] == session["thread_id"]
            assert (profile / "config.toml").read_text() == original
            with sync_playwright() as pw:
                browser = pw.chromium.launch(executable_path=os.environ.get("CHROME","/run/current-system/sw/bin/google-chrome"),headless=True,args=["--no-sandbox"])
                page = browser.new_page(viewport={"width":390,"height":844})
                page.goto(f"http://127.0.0.1:{port}")
                page.get_by_role("button",name="+ connection",exact=True).click()
                page.get_by_label("Access token").fill(token)
                page.get_by_role("button",name="Save and connect",exact=True).click()
                expect(page.locator("header .indicator")).to_have_text("connected to",timeout=20000)
                page.get_by_role("button",name="+ Session",exact=True).first.click()
                creation = page.get_by_role("dialog",name="New Session",exact=True)
                checkbox = creation.get_by_label("Default context window",exact=True)
                slider = creation.get_by_role("slider",name="Context window tokens",exact=True)
                expect(checkbox).to_be_checked()
                expect(checkbox).to_be_enabled(timeout=20000)
                expect(slider).to_have_count(0)
                checkbox.uncheck()
                expect(slider).to_be_enabled()
                expect(slider).to_have_value("272000")
                expect(slider).to_have_attribute("max","872000")
                slider.fill("500000")
                expect(creation.get_by_label("Context window value",exact=True)).to_have_text("500000")
                checkbox.check()
                expect(slider).to_have_count(0)
                # Catalogue limits need not be multiples of 1,000. The range
                # must show the same exact value that the form will submit.
                creation.get_by_label("Model", exact=True).select_option(smaller["slug"])
                checkbox.uncheck()
                expect(slider).to_have_value("128123")
                expect(creation.get_by_label("Context window value",exact=True)).to_have_text("128123")
                slider.fill("128122")
                expect(creation.get_by_label("Context window value",exact=True)).to_have_text("128122")
                checkbox.check()
                creation.get_by_role("button",name="Close",exact=True).click()
                page.locator(f'[data-session-id="{sid}"]').first.click()
                page.get_by_role("button",name="Session controls",exact=True).click()
                controls = page.get_by_role("dialog",name="Session controls",exact=True)
                checkbox = controls.get_by_label("Default context window",exact=True)
                slider = controls.get_by_role("slider",name="Context window tokens",exact=True)
                expect(checkbox).to_be_checked()
                expect(checkbox).to_be_enabled(timeout=20000)
                expect(slider).to_have_count(0)
                checkbox.uncheck()
                slider.fill("500000")
                controls.get_by_role("button",name="Apply context window",exact=True).click()
                expect(slider).to_be_enabled(timeout=20000)
                assert setting()["window"] == 500000
                if os.environ.get("DEMODEX_CONTEXT_SCREENSHOT"):
                    controls.get_by_label("Default context window",exact=True).scroll_into_view_if_needed()
                    page.screenshot(path=os.environ["DEMODEX_CONTEXT_SCREENSHOT"])
                page.reload()
                page.get_by_role("button",name="Session controls",exact=True).click()
                controls = page.get_by_role("dialog",name="Session controls",exact=True)
                expect(controls.get_by_label("Default context window",exact=True)).not_to_be_checked()
                expect(controls.get_by_role("slider",name="Context window tokens",exact=True)).to_have_value("500000")
                controls.get_by_label("Default context window",exact=True).check()
                controls.get_by_role("button",name="Apply context window",exact=True).click()
                expect(controls.get_by_role("button",name="Apply context window",exact=True)).to_be_enabled(timeout=20000)
                assert setting()["window"] is None
                assert page.evaluate("document.documentElement.scrollWidth <= innerWidth")
                browser.close()
            events = api({"Events":{"id":sid,"after":0}})
            assert not any(e["message"].get("method") == "turn/started" for e in events), events
            assert (profile / "config.toml").read_text() == original
            print("Context window lifecycle and mobile checkbox/slider passed; no inference.")
        finally:
            child.send_signal(signal.SIGTERM)
            try: child.wait(timeout=20)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGTERM)
                child.wait(timeout=10)
