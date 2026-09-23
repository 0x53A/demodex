# /// script
# dependencies = ["playwright", "websockets>=15"]
# ///
"""Shared target picker through real Yew/Wormhole, using a no-inference Codex fixture."""
import json
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request
from playwright.sync_api import sync_playwright, expect
from websockets.sync.server import serve
from websockets.exceptions import ConnectionClosed

ROOT = Path(__file__).resolve().parents[1]


def port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


calls = []
active = False


def codex(ws):
    global active
    try:
        for raw in ws:
            request = json.loads(raw)
            if 'id' not in request: continue
            calls.append(request)
            method = request['method']
            result = {}
            if method == 'thread/start':
                result = {'thread': {'id': 'fixture-thread', 'turns': []}, 'sandbox': {'type': 'dangerFullAccess'}}
            elif method == 'config/read': result = {'config': {}}
            elif method == 'thread/read': result = {'thread': {'status': {'type': 'active' if active else 'idle'}}}
            elif method == 'thread/queue/list': result = {'data': [], 'nextCursor': None}
            elif method == 'thread/goal/get': result = {'goal': {'status':'paused','objective':'Fixture goal','tokensUsed':0,'timeUsedSeconds':0,'tokenBudget':None}}
            elif method == 'model/list': result = {'data': [], 'nextCursor': None}
            elif method == 'turn/start':
                active = True
                result = {'turn': {'id': 'fixture-turn'}}
            elif method == 'turn/interrupt':
                active = False
                ws.send(json.dumps({'method': 'turn/completed', 'params': {'turn': {'id': 'fixture-turn'}}}))
            ws.send(json.dumps({'id': request['id'], 'result': result}))
            if method == 'turn/start':
                ws.send(json.dumps({'method':'turn/started','params':{'turn':{'id':'fixture-turn'}}}))
    except ConnectionClosed:
        pass


with tempfile.TemporaryDirectory(prefix='demodex-targets-') as temporary:
    root = Path(temporary)
    codex_port, daemon_port = port(), port()
    server = serve(codex, '127.0.0.1', codex_port)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    origin = f'http://127.0.0.1:{daemon_port}'
    with (root/'daemon.log').open('w+') as log:
        daemon = subprocess.Popen([str(ROOT/'target/rust-pwa/debug/demodex'), '--bind', f'127.0.0.1:{daemon_port}', '--data-dir', str(root/'state'), '--web-dir', str(ROOT/'web/.rust-dist')], stdout=log, stderr=log)
        try:
            for _ in range(100):
                try:
                    with socket.create_connection(('127.0.0.1', daemon_port), timeout=.1): break
                except OSError: time.sleep(.1)
            token = (root/'state/access-token').read_text().strip()

            def api(path, body=None):
                request = urllib.request.Request(origin+'/api'+path, data=None if body is None else json.dumps(body).encode(), headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'})
                return json.load(urllib.request.urlopen(request))

            session = api('/sessions', {'name':'Target fixture','endpoint':f'ws://127.0.0.1:{codex_port}','targets':[{'id':'First','url':'ws://127.0.0.1:5011','cwd':'/first'}]})
            api('/sessions/'+session['id']+'/connect', {})
            with sync_playwright() as playwright:
                browser = playwright.chromium.launch(executable_path='/run/current-system/sw/bin/google-chrome',headless=True,args=['--no-sandbox'])
                page = browser.new_page(viewport={'width':390,'height':844})
                errors=[]
                page.on('pageerror',lambda error:errors.append(str(error)))
                page.goto(origin)
                page.get_by_label('Access token').fill(token)
                page.get_by_role('button',name='Connect host',exact=True).click()
                expect(page.locator('header .indicator')).to_have_text('CONNECTED',timeout=20000)
                page.get_by_role('button',name='Server settings',exact=True).click()
                page.get_by_text('Register an external executor',exact=True).click()
                page.get_by_label('Target name',exact=True).fill('Second')
                page.get_by_label('Executor WebSocket URL',exact=True).fill('ws://127.0.0.1:5012')
                page.get_by_label('Default working directory',exact=True).fill('/second')
                page.get_by_role('button',name='Register target',exact=True).click()
                expect(page.locator('.target-registry')).to_contain_text('Second')
                # On mobile the session tree may be hidden; return through the header.
                page.set_viewport_size({'width':1200,'height':850})
                page.get_by_role('button',name='Target fixture',exact=False).click()
                page.get_by_label('Message',exact=True).fill('Draft kept while changing targets')
                page.get_by_role('button',name='Session controls',exact=True).click()
                dialog=page.get_by_role('dialog',name='Session controls',exact=True)
                dialog.locator('.target-picker summary').click()
                dialog.get_by_label('Second · external',exact=True).check()
                dialog.get_by_label('Second working directory',exact=True).fill('/second/project')
                dialog.get_by_role('button',name='Make primary',exact=True).click()
                dialog.get_by_role('button',name='Save targets',exact=True).click()
                expect(dialog.locator('.target-picker')).to_contain_text('Targets saved. Send a message')
                expect(dialog.get_by_role('button',name='Start / resume goal',exact=True)).to_be_disabled()
                detail=api('/sessions/'+session['id'])
                assert [t['cwd'] for t in detail['target_selection']]==['/second/project','/first'],detail
                assert len(detail['session']['targets'])==2 and detail['targets_pending']
                assert not any(c['method']=='turn/start' for c in calls)
                dialog.get_by_role('button',name='Close',exact=True).click()
                expect(page.get_by_label('Message',exact=True)).to_have_value('Draft kept while changing targets')
                page.get_by_role('button',name='Send',exact=True).click()
                expect(page.locator('.session-heading .status')).to_have_text('working')
                turn=next(c for c in calls if c['method']=='turn/start')
                assert [t['cwd'] for t in turn['params']['environments']]==['/second/project','/first']
                assert turn['params']['threadId']=='fixture-thread'
                page.get_by_role('button',name='Session controls',exact=True).click()
                dialog.locator('.target-picker summary').click()
                expect(dialog.get_by_role('button',name='Save targets',exact=True)).to_be_disabled()
                dialog.get_by_role('button',name='Close',exact=True).click()
                page.get_by_role('button',name='Interrupt',exact=True).click()
                expect(page.locator('.session-heading .status')).to_have_text('idle')
                page.get_by_role('button',name='Session controls',exact=True).click()
                dialog.locator('.target-picker summary').click()
                expect(dialog.get_by_role('button',name='Start / resume goal',exact=True)).to_be_enabled()
                expect(dialog.get_by_label('Second · external',exact=True)).to_be_checked()
                dialog.get_by_label('Second · external',exact=True).click()
                expect(dialog.get_by_label('Second · external',exact=True)).not_to_be_checked()
                dialog.get_by_label('First · external',exact=True).click()
                expect(dialog.get_by_label('First · external',exact=True)).not_to_be_checked()
                dialog.get_by_role('button',name='Save targets',exact=True).click()
                expect(dialog.locator('.target-picker')).to_contain_text('Targets saved. Send a message')
                assert api('/sessions/'+session['id'])['target_selection']==[]
                page.set_viewport_size({'width':390,'height':844})
                assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
                assert not errors,errors
                browser.close()
            print('PASS: shared registry, mobile target picker, primary/cwd, draft preservation, active lockout, next-turn selection and explicit empty selection')
        except BaseException:
            log.flush();log.seek(0);print(log.read()[-5000:])
            raise
        finally:
            daemon.terminate();daemon.wait(timeout=30)
            server.shutdown()
