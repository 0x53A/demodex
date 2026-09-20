# /// script
# dependencies = ["playwright"]
# ///
"""Check an actual built PWA at its public path, including offline installation."""
import argparse
import functools
import http.server
import json
from pathlib import Path
import shutil
import tempfile
import threading
from playwright.sync_api import sync_playwright, expect

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--dist", type=Path, default=ROOT / "web/.pages-dist")
parser.add_argument("--public-url", default="/demodex/")
parser.add_argument("--same-origin-host", action="store_true")
args = parser.parse_args()


class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


with tempfile.TemporaryDirectory(prefix="demodex-pages-") as temporary:
    site = Path(temporary) / args.public_url.strip("/")
    shutil.copytree(args.dist, site, dirs_exist_ok=True)
    server = http.server.ThreadingHTTPServer(
        ("127.0.0.1", 0), functools.partial(Handler, directory=temporary))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    origin = f"http://127.0.0.1:{server.server_port}"
    url = origin + args.public_url
    try:
        with sync_playwright() as playwright:
            system_chrome = "/run/current-system/sw/bin/google-chrome"
            browser = playwright.chromium.launch(
                executable_path=system_chrome if Path(system_chrome).exists() else None,
                headless=True, args=["--no-sandbox"])
            context = browser.new_context(viewport={"width": 390, "height": 844})
            page = context.new_page()
            errors, failures, sockets = [], [], []
            page.on("pageerror", lambda error: errors.append(str(error)))
            page.on("response", lambda response: failures.append(response.url) if response.status >= 400 else None)
            page.on("websocket", lambda socket: sockets.append(socket.url))
            page.goto(url)
            expect(page.get_by_role("heading", name="Your connections")).to_be_visible()
            expect(page.get_by_label("Host URL")).to_have_value(origin if args.same_origin_host else "")
            assert page.locator("a.brand").evaluate("a => a.href") == url
            page.evaluate("navigator.serviceWorker.ready")
            page.wait_for_function("navigator.serviceWorker.controller !== null")
            assert page.evaluate("async () => (await navigator.serviceWorker.ready).scope") == url
            manifest_url = page.locator('link[rel="manifest"]').evaluate("e => e.href")
            assert manifest_url == url + "manifest.webmanifest"
            manifest = json.loads((site / "manifest.webmanifest").read_text())
            assert manifest["start_url"] == "./" and manifest["scope"] == "./"
            for icon in manifest["icons"]:
                assert (site / icon["src"]).is_file()
            cached = page.evaluate("async () => (await Promise.all((await caches.keys()).map(async key => (await (await caches.open(key)).keys()).map(r => r.url)))).flat()")
            assert cached and all(asset.startswith(url) for asset in cached)
            assert any(asset.endswith(".wasm") for asset in cached)
            context.set_offline(True)
            page.reload()
            expect(page.get_by_role("heading", name="Your connections")).to_be_visible()
            assert not sockets, sockets
            assert not errors, errors
            assert not failures, failures
            browser.close()
        print(f"PASS: {args.public_url} assets, manifest, worker scope, default host and offline launch")
    finally:
        server.shutdown()
        server.server_close()
