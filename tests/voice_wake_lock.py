# /// script
# dependencies = ["playwright"]
# ///
"""Real browser capture with fake microphone and controlled wake-lock outcomes."""
import functools
import http.server
import os
from pathlib import Path
import threading
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[1]

class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass

with http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Quiet, directory=str(ROOT))) as server:
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with sync_playwright() as pw:
        browser = pw.chromium.launch(
            executable_path=os.environ.get('CHROME', '/run/current-system/sw/bin/google-chrome'),
            headless=True, args=['--no-sandbox', '--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream'])
        for mode in ('stop', 'cancel', 'hidden', 'track-ended', 'pending', 'denied', 'unsupported', 'revoked'):
            page = browser.new_page()
            errors = []
            page.on('pageerror', lambda e: errors.append(str(e)))
            page.goto(f'http://127.0.0.1:{server.server_port}/')
            page.evaluate('''async mode => {
                window.api = await import('/crates/web/src/voice.js');
                window.messages = []; window.warnings = []; window.requests = 0;
                window.releases = 0; window.tracks = [];
                const get = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
                navigator.mediaDevices.getUserMedia = async opts => {
                    const stream = await get(opts); tracks.push(...stream.getTracks()); return stream;
                };
                window.lock = new EventTarget(); lock.released = false;
                lock.release = async () => { releases++; lock.released = true; lock.dispatchEvent(new Event('release')); };
                Object.defineProperty(navigator, 'wakeLock', {configurable: true, value: mode === 'unsupported' ? undefined : {
                    request: async type => {
                        if (type !== 'screen') throw new Error('Wrong wake lock type');
                        requests++;
                        if (mode === 'denied') throw new DOMException('Denied', 'NotAllowedError');
                        if (mode === 'pending') await new Promise(resolve => window.grant = resolve);
                        return lock;
                    }
                }});
                window.controller = await api.prepareRecording(x => messages.push(x), x => warnings.push(x));
                if (requests !== 0) throw new Error('Lock acquired before recording');
                api.startRecording(controller);
            }''', mode)
            page.wait_for_function('messages.some(x => x instanceof Int16Array)')
            assert page.evaluate('requests') == (0 if mode == 'unsupported' else 1)
            if mode == 'stop':
                page.evaluate('api.stopRecording(controller)')
            elif mode == 'hidden':
                page.evaluate("Object.defineProperty(document, 'hidden', {value: true}); document.dispatchEvent(new Event('visibilitychange'))")
                assert 'page was hidden' in page.evaluate('messages.find(x => typeof x === "string")')
            elif mode == 'track-ended':
                page.evaluate("tracks[0].dispatchEvent(new Event('ended'))")
                assert 'Microphone disconnected' in page.evaluate('messages.find(x => typeof x === "string")')
            else:
                if mode in ('denied', 'unsupported'):
                    page.wait_for_function('warnings.length === 1')
                    assert page.evaluate('tracks.every(t => t.readyState === "live")')
                if mode == 'revoked':
                    page.evaluate('lock.release()')
                    assert page.evaluate('warnings.length') == 1
                    assert page.evaluate('tracks.every(t => t.readyState === "live")')
                page.evaluate('api.cancelRecording(controller)')
                if mode == 'pending':
                    assert page.evaluate('releases') == 0
                    page.evaluate('grant()')
            page.wait_for_function('tracks.every(t => t.readyState === "ended")')
            expected = 0 if mode in ('denied', 'unsupported') else (2 if mode == 'revoked' else 1)
            page.wait_for_function(f'releases === {expected}')
            if mode not in ('denied', 'unsupported', 'revoked'):
                assert page.evaluate('warnings.length') == 0
            assert not errors, errors
            page.close()
            print(f'{mode}: passed')
        browser.close()
    server.shutdown()
