# /// script
# dependencies = ["playwright"]
# ///
"""Real service-worker lifecycle on a disposable origin. No model or deployment.

uv run tests/pwa.py
"""
import functools
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import shutil
import tempfile
import threading
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[1]
class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def handle(self):
        try:
            super().handle()
        except (BrokenPipeError, ConnectionResetError):
            pass  # Reload/offline transitions deliberately cancel requests.


def release(root, label):
    index = root / 'index.html'
    index.write_text(re.sub(r'<body(?: data-version="[^"]*")?>', f'<body data-version="{label}">', index.read_text()))
    files = sorted(p for p in root.rglob('*') if p.is_file() and p.name != 'service-worker.js')
    assets = [{'path': './' + p.relative_to(root).as_posix(), 'hash': hashlib.sha256(p.read_bytes()).hexdigest()} for p in files]
    template = (ROOT / 'crates/web/service-worker.js').read_text()
    version = hashlib.sha256((json.dumps(assets) + template).encode()).hexdigest()
    (root / 'service-worker.js').write_text('const BUILD = ' + json.dumps({'version': version, 'assets': assets}) + ';\n' + template)


with tempfile.TemporaryDirectory(prefix='demodex-pwa-') as directory:
    root = Path(directory)
    shutil.copytree(Path(os.environ.get('DEMODEX_WEB_DIR', ROOT / 'web/.rust-dist')), root, dirs_exist_ok=True)
    release(root, 'one')
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Handler, directory=directory))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    origin = f'http://127.0.0.1:{server.server_port}/'
    try:
        with sync_playwright() as p:
            browser = p.chromium.launch(executable_path='/run/current-system/sw/bin/google-chrome', headless=True, args=['--no-sandbox'])
            context = browser.new_context(viewport={'width': 390, 'height': 844})
            page = context.new_page()
            # Restore a previously opened form. A fresh unauthenticated window
            # correctly starts at the connection picker; this static-only fixture
            # has no daemon with which to complete login and navigate there.
            page.add_init_script('''if (!sessionStorage.getItem('pwa-fixture-seeded')) {
                sessionStorage.setItem('pwa-fixture-seeded', '1');
                sessionStorage.setItem('demodex-rust-view', JSON.stringify({host:location.origin,page:'environments'}));
                history.replaceState(JSON.stringify({host:location.origin,selected:'',page:'environments',connections:false}), '');
            }''')
            errors = []
            page.on('pageerror', lambda error: errors.append(str(error)))
            page.goto(origin)
            page.get_by_role('heading', name='Server settings', exact=True).wait_for()
            page.get_by_text('Add SSH target', exact=True).click()
            page.get_by_label('SSH target name', exact=True).fill('Unsubmitted session')
            page.evaluate('navigator.serviceWorker.ready')
            page.wait_for_function('navigator.serviceWorker.controller !== null')
            page.evaluate("window.marker=1; caches.open('unrelated-app')")
            other = context.new_page()
            other.goto(origin)
            other.get_by_label('Access token').wait_for()
            other.evaluate('window.marker=2')
            release(root, 'two')
            page.evaluate("window.dispatchEvent(new Event('pageshow'))")
            page.get_by_role('button', name='Update now', exact=True).wait_for(timeout=20_000)
            assert page.locator('body').get_attribute('data-version') == 'one'
            assert page.evaluate('window.marker') == 1
            assert page.get_by_label('SSH target name', exact=True).input_value() == 'Unsubmitted session'
            assert page.evaluate('document.documentElement.scrollHeight <= innerHeight')
            with page.expect_navigation():
                page.get_by_role('button', name='Update now', exact=True).click()
            page.get_by_role('heading', name='Server settings', exact=True).wait_for()
            page.get_by_text('Add SSH target', exact=True).click()
            assert page.locator('body').get_attribute('data-version') == 'two'
            assert page.get_by_label('SSH target name', exact=True).input_value() == 'Unsubmitted session'
            assert other.evaluate('window.marker') == 2, 'another tab was forcibly reloaded'
            other.get_by_role('button', name='Update now', exact=True).wait_for()
            release(root, 'three')
            launched = context.new_page()
            launched.goto(origin)
            launched.wait_for_function('document.body.dataset.version === "three"', timeout=20_000)
            launched.get_by_label('Access token').wait_for()
            assert other.evaluate('window.marker') == 2
            # A corrupt/partial release must never replace the working shell.
            release(root, 'broken')
            (root / 'index.html').write_text('partial deployment')
            launched.evaluate('''async () => {
                const r = await navigator.serviceWorker.getRegistration();
                await new Promise(async resolve => {
                    r.addEventListener('updatefound', () => {
                        const w = r.installing;
                        w.addEventListener('statechange', () => {
                            if (w.state === 'redundant') resolve();
                        });
                    }, {once:true});
                    await r.update();
                });
            }''')
            assert launched.get_by_role('button', name='Update now', exact=True).count() == 0
            assert launched.locator('body').get_attribute('data-version') == 'three'
            cached = page.evaluate('''async () => {
                const urls=[];
                for (const key of await caches.keys()) for (const r of await (await caches.open(key)).keys()) urls.push(r.url);
                return urls;
            }''')
            assert cached and not any('/api/' in url for url in cached), cached
            assert page.evaluate("caches.has('unrelated-app')")
            # Offline reload serves only the shell; API data must not masquerade as live.
            context.set_offline(True)
            launched.reload()
            launched.get_by_label('Access token').wait_for(timeout=15_000)
            assert launched.locator('body').get_attribute('data-version') == 'three'
            assert launched.get_by_role('heading', name='PWA fixture', exact=True).count() == 0
            context.set_offline(False)
            assert page.get_by_label('SSH target name', exact=True).input_value() == 'Unsubmitted session'
            assert errors == [], errors
            browser.close()
            print('PASS: update notice, explicit reload, launch update, other tabs preserved, form recovery, partial release rejected, offline shell, static-only cache')
    finally:
        server.shutdown()
        server.server_close()
