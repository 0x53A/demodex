# /// script
# dependencies = ["playwright", "websockets>=15"]
# ///
"""Shared target picker through real Yew/Wormhole, using a no-inference Codex fixture."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request
import uuid
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
interrupt_gate = False
interrupt_release = threading.Event()
interrupt_requested = threading.Event()
interrupt_reject = False


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
            elif method == 'turn/steer': result = {'turnId':'fixture-turn'}
            elif method == 'turn/interrupt':
                if interrupt_reject:
                    ws.send(json.dumps({'id':request['id'],'error':{'code':-1,'message':'Fixture interrupt rejected'}}))
                    continue
                if interrupt_gate:
                    ws.send(json.dumps({'id':request['id'],'result':{}}))
                    interrupt_requested.set()
                    def finish():
                        global active
                        if not interrupt_release.wait(20): return
                        active = False
                        ws.send(json.dumps({'method':'turn/completed','params':{'turn':{'id':'fixture-turn','status':'interrupted'}}}))
                    threading.Thread(target=finish,daemon=True).start()
                    continue
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
        daemon = subprocess.Popen([os.environ.get('DEMODEX_BIN',str(ROOT/'target/rust-pwa/debug/demodex')), '--bind', f'127.0.0.1:{daemon_port}', '--data-dir', str(root/'state'), '--web-dir', str(ROOT/'web/.rust-dist')], stdout=log, stderr=log)
        try:
            for _ in range(100):
                try:
                    with socket.create_connection(('127.0.0.1', daemon_port), timeout=.1): break
                except OSError: time.sleep(.1)
            token = (root/'state/access-token').read_text().strip()

            def api(path, body=None):
                from wormhole_client import api as actor_api
                return actor_api(origin, token, path, body, None)

            session = api('/sessions', {'name':'Target fixture','endpoint':f'ws://127.0.0.1:{codex_port}','targets':[{'id':'First','url':'ws://127.0.0.1:5011','cwd':'/first'}]})
            api('/sessions/'+session['id']+'/connect', {})
            with sync_playwright() as playwright:
                browser = playwright.chromium.launch(executable_path=os.environ.get('CHROME','/run/current-system/sw/bin/google-chrome'),headless=True,args=['--no-sandbox'])
                page = browser.new_page(viewport={'width':390,'height':844})
                errors=[]
                page.on('pageerror',lambda error:errors.append(str(error)))
                page.goto(origin)
                page.get_by_role('button',name='+ connection',exact=True).click()
                page.get_by_label('Access token').fill(token)
                page.get_by_role('button',name='Save and connect',exact=True).click()
                expect(page.locator('header .indicator')).to_have_text('connected to',timeout=20000)
                page.get_by_role('button',name='Server settings',exact=True).click()
                page.get_by_role('button',name='+ Register external executor',exact=True).click()
                page.get_by_label('Executor name',exact=True).fill('Second')
                page.get_by_label('Executor WebSocket URL',exact=True).fill('ws://127.0.0.1:5012')
                page.get_by_label('Default working directory',exact=True).fill('/second')
                page.get_by_role('button',name='Register executor',exact=True).click()
                expect(page.locator('.target-registry')).to_contain_text('Second')
                page.get_by_role('dialog',name='Server settings',exact=True).get_by_role('button',name='Close',exact=True).click()
                # On mobile the session tree may be hidden; return through the header.
                page.set_viewport_size({'width':1200,'height':850})
                page.locator('.session').filter(has_text='Target fixture').click()
                page.get_by_label('Message',exact=True).fill('Draft kept while changing targets')
                page.get_by_role('button',name='Session controls',exact=True).click()
                dialog=page.get_by_role('dialog',name='Session controls',exact=True)
                dialog.get_by_label('Second · external',exact=True).check()
                dialog.get_by_label('Second working directory',exact=True).fill('/second/project')
                dialog.get_by_role('button',name='Make primary',exact=True).click()
                dialog.get_by_role('button',name='Save executors and directories',exact=True).click()
                expect(dialog.locator('.target-picker')).to_contain_text('Executors and directories saved. Send a message')
                expect(dialog.get_by_role('button',name='Start / resume goal',exact=True)).to_be_disabled()
                expect(dialog.get_by_role('button',name='Start goal',exact=True)).to_have_count(0)
                dialog.get_by_label('Goal objective',exact=True).fill('Replacement goal')
                expect(dialog.get_by_role('button',name='Start goal',exact=True)).to_be_disabled()
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
                expect(dialog.get_by_role('button',name='Save for next turn',exact=True)).to_be_enabled()
                expect(dialog.get_by_role('button',name='Interrupt and save',exact=True)).to_be_enabled()
                page.set_viewport_size({'width':390,'height':844})
                dialog.get_by_role('button',name='Interrupt and save',exact=True).scroll_into_view_if_needed()
                page.screenshot(path=str(ROOT/'target'/'review-target-actions-mobile.png'))
                page.set_viewport_size({'width':1200,'height':850})
                starts=sum(c['method']=='turn/start' for c in calls)
                interrupts=sum(c['method']=='turn/interrupt' for c in calls)
                # Directory-only edits are visible in Session Controls and can
                # be staged while work continues, without a session restart.
                expect(dialog.get_by_role('heading',name='Working directories',exact=True)).to_be_visible()
                dialog.get_by_label('Second (primary) working directory',exact=True).fill('/second/next-project')
                dialog.get_by_role('button',name='Save for next turn',exact=True).click()
                expect(dialog.locator('.effective-targets')).to_contain_text('/second/project')
                detail=api('/sessions/'+session['id'])
                assert detail['target_selection'][0]['cwd']=='/second/next-project',detail
                assert detail['session']['targets'][0]['cwd']=='/second/project',detail
                assert detail['session']['thread_id']=='fixture-thread'
                assert sum(c['method']=='turn/start' for c in calls)==starts
                assert sum(c['method']=='turn/interrupt' for c in calls)==interrupts
                assert not any(c['method']=='thread/settings/update' and 'cwd' in c['params'] for c in calls)
                # Save a reduction without changing the running turn or its users.
                dialog.get_by_label('Second · external',exact=True).click()
                expect(dialog.get_by_label('Second · external',exact=True)).not_to_be_checked()
                dialog.get_by_role('button',name='Save for next turn',exact=True).click()
                expect(dialog.locator('.effective-targets')).to_contain_text('/second/project')
                detail=api('/sessions/'+session['id'])
                assert len(detail['target_selection'])==1 and detail['targets_pending']
                assert len(detail['session']['targets'])==2
                assert sum(c['method']=='turn/interrupt' for c in calls)==interrupts
                assert sum(c['method']=='turn/start' for c in calls)==starts
                # Reload keeps the accepted pending selection and the active turn.
                page.reload()
                expect(page.locator('.session-heading .status')).to_have_text('working')
                expect(page.locator('.session-inbox')).to_contain_text('This turn keeps its current targets')
                expect(page.get_by_role('button',name='Attach image',exact=True)).to_be_disabled()
                page.get_by_label('Message',exact=True).fill('Steer existing work')
                page.get_by_role('button',name='Send',exact=True).click()
                expect(page.get_by_label('Message',exact=True)).to_have_value('')
                assert calls[-1]['method'] != 'turn/start'
                assert api('/sessions/'+session['id'])['targets_pending']
                page.get_by_role('button',name='Session controls',exact=True).click()
                expect(dialog.get_by_label('Second · external',exact=True)).not_to_be_checked()
                # A rejected interrupt leaves both saved and effective selections alone.
                dialog.get_by_label('First · external',exact=True).click()
                expect(dialog.get_by_label('First · external',exact=True)).not_to_be_checked()
                interrupt_reject=True
                dialog.get_by_role('button',name='Interrupt and save',exact=True).click()
                expect(dialog.get_by_role('alert')).to_contain_text('targets were not saved')
                interrupt_reject=False
                detail=api('/sessions/'+session['id'])
                assert len(detail['target_selection'])==1 and len(detail['session']['targets'])==2
                dialog.get_by_role('button',name='Dismiss',exact=True).click()
                # An accepted interrupt is not completion: no selection is saved yet.
                interrupt_gate=True
                dialog.get_by_role('button',name='Interrupt and save',exact=True).click()
                assert interrupt_requested.wait(5)
                expect(dialog.get_by_role('button',name='Interrupt and save',exact=True)).to_be_disabled()
                detail=api('/sessions/'+session['id'])
                assert len(detail['target_selection'])==1 and len(detail['session']['targets'])==2
                interrupt_release.set()
                interrupt_gate=False
                expect(page.locator('.session-heading .status')).to_have_text('idle')
                expect(dialog.get_by_role('button',name='Save executors and directories',exact=True)).to_be_enabled()
                detail=api('/sessions/'+session['id'])
                assert detail['target_selection']==[] and detail['targets_pending']
                assert len(detail['session']['targets'])==2, 'saving claimed immediate revocation'
                assert sum(c['method']=='turn/start' for c in calls)==starts, 'hidden turn started'
                dialog.get_by_role('button',name='Close',exact=True).click()
                page.get_by_label('Message',exact=True).fill('Continue with no executors')
                page.get_by_role('button',name='Send',exact=True).click()
                expect(page.locator('.session-heading .status')).to_have_text('working')
                last_turn=next(c for c in reversed(calls) if c['method']=='turn/start')
                assert last_turn['params']['environments']==[]
                detail=api('/sessions/'+session['id'])
                assert detail['session']['targets']==[] and not detail['targets_pending']
                # Duplicate interruption receipts cannot interrupt a later turn.
                from wormhole_client import call
                receipt=str(uuid.uuid4())
                command={'ChangeTargets':{'id':session['id'],'input':{'targets':[]},'mode':'Interrupt'}}
                result=call(origin,token,command,receipt)
                api('/sessions/'+session['id']+'/messages',{'text':'A later turn'})
                interrupts=sum(c['method']=='turn/interrupt' for c in calls)
                assert call(origin,token,command,receipt)==result
                assert sum(c['method']=='turn/interrupt' for c in calls)==interrupts
                assert active
                # An interrupt acknowledgement with no completion expires without
                # saving. A late completion must not revive the expired operation.
                interrupt_gate=True
                interrupt_release.clear()
                interrupt_requested.clear()
                first=next(t for t in api('/targets') if t['name']=='First')
                timeout_command={'ChangeTargets':{'id':session['id'],'input':{'targets':[{'id':first['id'],'cwd':'/first'}]},'mode':'Interrupt'}}
                try:
                    call(origin,token,timeout_command,str(uuid.uuid4()))
                    raise AssertionError('Unconfirmed interrupt succeeded')
                except AssertionError as exc:
                    assert 'Interruption is still unconfirmed' in str(exc),str(exc)
                assert active
                assert api('/sessions/'+session['id'])['target_selection']==[]
                interrupt_release.set()
                interrupt_gate=False
                expect(page.locator('.session-heading .status')).to_have_text('idle')
                detail=api('/sessions/'+session['id'])
                assert detail['target_selection']==[] and not detail['targets_pending']
                page.set_viewport_size({'width':390,'height':844})
                assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
                assert not errors,errors
                browser.close()
            print('PASS: shared registry, mobile target picker, primary/cwd, draft preservation, active staging, confirmed interruption, receipts, next-turn selection and explicit empty selection')
        except BaseException:
            log.flush();log.seek(0);print(log.read()[-5000:])
            raise
        finally:
            daemon.terminate();daemon.wait(timeout=30)
            server.shutdown()
