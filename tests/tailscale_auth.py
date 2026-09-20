# /// script
# dependencies = ["playwright", "aiohttp", "websockets>=15"]
# ///
"""Isolated browser authentication through a Unix proxy; no Codex or model turns."""
import asyncio
import json
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request
import urllib.error
from aiohttp import web, ClientSession, UnixConnector, WSMsgType
from playwright.sync_api import sync_playwright, expect
from websockets.sync.client import unix_connect
from websockets.exceptions import InvalidStatus

ROOT = Path(__file__).resolve().parents[1]
def port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]

with tempfile.TemporaryDirectory(prefix='demodex-identity-') as temporary:
    state = Path(temporary) / 'state'
    daemon_port, proxy_port = port(), port()
    origin = f'http://127.0.0.1:{proxy_port}'
    direct = f'http://127.0.0.1:{daemon_port}'
    identity = ['owner@example.com']
    log = (Path(temporary) / 'daemon.log').open('w+')
    daemon = subprocess.Popen([str(ROOT / 'target/rust-pwa/debug/demodex'),
        '--bind', f'127.0.0.1:{daemon_port}', '--data-dir', str(state),
        '--api-only', '--allowed-origin', origin, '--tailscale-user', identity[0]],
        cwd=ROOT, stdout=log, stderr=log)
    loop = asyncio.new_event_loop()
    ready = threading.Event()

    async def handler(request):
        if request.path != '/wormhole':
            path = 'index.html' if request.path == '/' else request.path.lstrip('/')
            return web.FileResponse(ROOT / 'web/.rust-dist' / path)
        # Model Serve's stripping/replacement of browser-supplied identity.
        headers = {'Origin': request.headers.get('Origin', '')}
        if identity[0] is not None:
            headers['Tailscale-User-Login'] = identity[0]
        async with ClientSession(connector=UnixConnector(path=str(state / 'tailscale.sock'))) as client:
            async with client.ws_connect('http://localhost/wormhole', headers=headers) as backend:
                frontend = web.WebSocketResponse()
                await frontend.prepare(request)
                async def relay(source, target):
                    async for message in source:
                        if message.type == WSMsgType.TEXT:
                            await target.send_str(message.data)
                        elif message.type == WSMsgType.BINARY:
                            await target.send_bytes(message.data)
                    await target.close()
                tasks = [asyncio.create_task(relay(frontend, backend)), asyncio.create_task(relay(backend, frontend))]
                try:
                    await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
                finally:
                    for task in tasks:
                        task.cancel()
                    await asyncio.gather(*tasks, return_exceptions=True)
                return frontend

    async def start_proxy():
        app = web.Application()
        app.router.add_route('GET', '/{path:.*}', handler)
        runner = web.AppRunner(app)
        await runner.setup()
        await web.TCPSite(runner, '127.0.0.1', proxy_port).start()
        ready.set()
        return runner

    def run_proxy():
        asyncio.set_event_loop(loop)
        loop.run_forever()

    thread = threading.Thread(target=run_proxy, daemon=True)
    thread.start()
    try:
        for _ in range(200):
            assert daemon.poll() is None
            if (state / 'tailscale.sock').exists():
                break
            time.sleep(.05)
        assert (state / 'tailscale.sock').stat().st_mode & 0o777 == 0o600
        runner = asyncio.run_coroutine_threadsafe(start_proxy(), loop).result(10)
        token = (state / 'access-token').read_text().strip()
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(executable_path='/run/current-system/sw/bin/google-chrome', headless=True, args=['--no-sandbox'])
            context = browser.new_context(service_workers='block')
            page = context.new_page()
            page.goto(origin)
            expect(page.locator('header .indicator')).to_have_text('CONNECTED', timeout=20000)
            assert all(c['token'] == '' for c in page.evaluate('JSON.parse(localStorage.getItem("demodex-connections"))'))
            page.reload()
            expect(page.locator('header .indicator')).to_have_text('CONNECTED', timeout=20000)
            context.close()
            print('PASS: identity login and reload without a saved token')

            for denied in ['other@example.com', None]:
                identity[0] = denied
                context = browser.new_context(service_workers='block', extra_http_headers={'Tailscale-User-Login': 'owner@example.com'})
                page = context.new_page()
                page.goto(origin)
                expect(page.get_by_role('alert')).to_contain_text('Access token required', timeout=20000)
                page.get_by_label('Access token').fill('invalid')
                page.get_by_role('button', name='Connect host', exact=True).click()
                expect(page.get_by_role('alert')).to_contain_text('Access token rejected', timeout=20000)
                page.get_by_label('Access token').fill(token)
                page.get_by_role('button', name='Connect host', exact=True).click()
                expect(page.locator('header .indicator')).to_have_text('CONNECTED', timeout=20000)
                context.close()
            print('PASS: denied/missing identities require tokens; invalid rejected, valid fallback accepted')

            context = browser.new_context(service_workers='block', extra_http_headers={'Tailscale-User-Login': 'owner@example.com'})
            page = context.new_page()
            page.goto(origin)
            expect(page.get_by_role('alert')).to_contain_text('Access token required', timeout=20000)
            page.get_by_label('Host URL').fill(direct)
            page.get_by_role('button', name='Connect host', exact=True).click()
            expect(page.get_by_role('alert')).to_contain_text('Access token required', timeout=20000)
            page.get_by_label('Access token').fill(token)
            page.get_by_role('button', name='Connect host', exact=True).click()
            expect(page.locator('header .indicator')).to_have_text('CONNECTED', timeout=20000)
            context.close()
            browser.close()
            print('PASS: forged TCP identity ignored; direct token fallback works')

        try:
            with unix_connect(str(state / 'tailscale.sock'), uri='ws://localhost/wormhole',
                origin='https://untrusted.example', additional_headers={'Tailscale-User-Login': 'owner@example.com'}):
                raise AssertionError('untrusted browser origin accepted')
        except InvalidStatus as error:
            assert error.response.status_code == 403
        try:
            urllib.request.urlopen(urllib.request.Request(direct + '/api/sessions', headers={'Tailscale-User-Login': 'owner@example.com'}))
            raise AssertionError('REST accepted forged identity')
        except urllib.error.HTTPError as error:
            assert error.code == 401
        print('PASS: untrusted origin denied; REST remains token-only')
    finally:
        if 'runner' in locals():
            asyncio.run_coroutine_threadsafe(runner.cleanup(), loop).result(15)
        loop.call_soon_threadsafe(loop.stop)
        thread.join(5)
        daemon.terminate()
        try:
            daemon.wait(15)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()
