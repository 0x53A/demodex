# /// script
# dependencies = ["playwright", "websockets>=15"]
# ///
"""New-session widget and runtime-owned target selection; no model inference."""
import json
import os
from pathlib import Path
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid
from playwright.sync_api import sync_playwright, expect
from websockets.sync.server import unix_serve
from websockets.exceptions import ConnectionClosed

ROOT = Path(__file__).resolve().parents[1]

if '--fixture' in sys.argv:
    endpoint = sys.argv[sys.argv.index('--listen') + 1].removeprefix('unix://')
    def handle(ws):
        try:
            for raw in ws:
                request = json.loads(raw)
                if 'id' not in request: continue
                with open(os.environ['DEMODEX_FIXTURE_CALLS'], 'a') as log:
                    log.write(json.dumps(request) + '\n')
                method, params = request['method'], request.get('params', {})
                result = {}
                if method == 'account/read': result = {'account': {'type': 'chatgpt', 'email': 'fixture@example.test', 'planType': 'pro'}}
                elif method == 'config/read': result = {'config': {}}
                elif method in ('thread/start', 'thread/resume'):
                    result = {'thread': {'id': params.get('threadId', str(uuid.uuid4())), 'turns': []}, 'sandbox': {'type': 'dangerFullAccess' if params.get('sandbox') == 'danger-full-access' else 'readOnly'}}
                elif method == 'thread/read': result = {'thread': {'status': {'type': 'idle'}}}
                elif method == 'thread/goal/get': result = {'goal': None}
                elif method in ('thread/queue/list', 'model/list', 'thread/list'): result = {'data': [], 'nextCursor': None}
                ws.send(json.dumps({'id': request['id'], 'result': result}))
        except ConnectionClosed: pass
    with unix_serve(handle, endpoint) as server: server.serve_forever()
    sys.exit(0)

with tempfile.TemporaryDirectory(prefix='demodex-new-session-') as temporary:
    root = Path(temporary)
    real_codex = shutil.which('codex')
    assert real_codex
    bin_dir = root/'bin'
    bin_dir.mkdir()
    wrapper = bin_dir/'codex'
    wrapper.write_text('#!/bin/sh\nif [ "$1" = app-server ]; then\n shift\n exec '+shlex.quote(sys.executable)+' '+shlex.quote(__file__)+' --fixture "$@"\nfi\nexec '+shlex.quote(real_codex)+' "$@"\n')
    wrapper.chmod(0o700)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    origin = f'http://127.0.0.1:{port}'
    env = os.environ | {'PATH': str(bin_dir)+os.pathsep+os.environ['PATH'], 'DEMODEX_FIXTURE_CALLS': str(root/'calls.jsonl')}
    log = (root/'daemon.log').open('w+')
    def launch():
        child = subprocess.Popen([str(ROOT/'target/rust-pwa/debug/demodex'), '--bind', f'127.0.0.1:{port}', '--data-dir', str(root/'state'), '--host-workspace', str(root), '--web-dir', str(ROOT/'web/.rust-dist')], env=env, stdout=log, stderr=log)
        for _ in range(200):
            assert child.poll() is None, (root/'daemon.log').read_text()
            try:
                with socket.create_connection(('127.0.0.1', port), timeout=.1): return child
            except OSError: time.sleep(.1)
        raise AssertionError('Daemon did not start')
    daemon = launch()
    token = (root/'state/access-token').read_text().strip()
    def api(path, body=None, error=None):
        request = urllib.request.Request(origin+'/api'+path, data=None if body is None else json.dumps(body).encode(), headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'})
        try:
            value = json.load(urllib.request.urlopen(request, timeout=30))
            assert error is None, value
            return value
        except urllib.error.HTTPError as exc:
            message = exc.read().decode()
            assert error and error in message, message
    try:
        target = api('/targets', {'name':'Build machine','url':'ws://127.0.0.1:5012','cwd':'/remote/project'})
        target_id = target['id']
        api('/runtime/sessions', {'name':'Bad','targets':[{'id':'missing','cwd':'/project'}]}, error='Unknown target')
        api('/runtime/sessions', {'name':'Bad','targets':[{'id':target_id,'cwd':'relative'}]}, error='absolute path')
        api('/runtime/sessions', {'name':'Bad','targets':[{'id':'ssh-missing','cwd':'/project'}],'sandbox':'read-only'}, error='danger-full-access')
        assert not api('/sessions')
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(executable_path='/run/current-system/sw/bin/google-chrome',headless=True,args=['--no-sandbox'])
            page = browser.new_page(viewport={'width':1200,'height':850})
            errors=[]
            page.on('pageerror',lambda error:errors.append(str(error)))
            page.goto(origin)
            page.get_by_label('Connection name (optional)').fill('Laptop')
            page.get_by_label('Access token').fill(token)
            page.get_by_role('button',name='Connect host',exact=True).click()
            expect(page.locator('header .current-server')).to_contain_text('Laptop')
            page.get_by_role('button',name='Server settings',exact=True).click()
            expect(page.locator('main').get_by_role('button',name='Create session',exact=True)).to_have_count(0)
            page.get_by_role('button',name='+ New Session',exact=True).click()
            dialog=page.get_by_role('dialog',name='New Session',exact=True)
            dialog.get_by_label('Session name',exact=True).fill('Remote only')
            dialog.get_by_label('Build machine · external',exact=True).check()
            expect(dialog.get_by_label('This host · host',exact=True)).not_to_be_checked()
            dialog.get_by_label('Build machine (primary) working directory',exact=True).fill('/remote/project')
            dialog.get_by_label('Sandbox',exact=True).select_option('read-only')
            dialog.get_by_text('Resume a saved session',exact=True).click()
            dialog.get_by_label('Resumed session name',exact=True).fill('Keep resume draft')
            dialog.get_by_label('Existing Codex thread ID',exact=True).fill('saved-thread')
            dialog.get_by_label('Resume sandbox',exact=True).select_option('workspace-write')
            expect(dialog.get_by_label('Session name',exact=True)).to_have_value('Remote only')
            expect(dialog.get_by_label('Sandbox',exact=True)).to_have_value('read-only')
            assert dialog.evaluate("(d) => { const ids=[...d.querySelectorAll('[id]')].map(e=>e.id); return new Set(ids).size===ids.length; }")
            dialog.get_by_text('Resume a saved session',exact=True).click()
            for width,height,label in [(1200,850,'desktop'),(390,844,'mobile')]:
                page.set_viewport_size({'width':width,'height':height})
                page.screenshot(path=str(ROOT/'target'/f'new-session-{label}.png'))
            page.set_viewport_size({'width':1200,'height':850})
            dialog.get_by_role('button',name='Create session',exact=True).click()
            expect(dialog).to_have_count(0)
            expect(page.locator('.session-heading .status')).to_have_text('connected')
            session = next(s for s in api('/sessions') if s['name']=='Remote only')
            detail = api('/sessions/'+session['id'])
            assert detail['target_selection']==[{'id':target_id,'cwd':'/remote/project'}], detail
            assert len(session['targets'])==1 and not session['targets'][0]['id'].startswith('host-'),session
            assert not detail['targets_pending']
            page.get_by_label('Message',exact=True).fill('Unsent conversation draft')
            # A second generation at the same path must share the folder entry.
            second=api('/runtime/sessions',{'name':'Same folder','targets':[{'id':target_id,'cwd':'/remote/project'}],'sandbox':'read-only'})
            expect(page.locator('aside .tree-agent')).to_have_count(2)
            expect(page.locator('aside .folder-name')).to_have_count(1)
            for width,height in [(1200,850),(390,844)]:
                page.set_viewport_size({'width':width,'height':height})
                if width==390: page.get_by_role('button',name='← Sessions',exact=True).click()
                page.get_by_role('button',name='+ New Session',exact=True).click()
                dialog.get_by_label('Session name',exact=True).fill('Keep this draft')
                dialog.get_by_text('Create a VM',exact=True).click()
                expect(dialog.get_by_label('Environment name',exact=True)).to_be_visible()
                dialog.get_by_label('Memory (MiB)',exact=True).fill('12')
                assert not dialog.get_by_label('Memory (MiB)',exact=True).evaluate('e=>e.checkValidity()')
                dialog.get_by_label('Memory (MiB)',exact=True).fill('4096')
                dialog.get_by_text('Resume a saved session',exact=True).click()
                expect(dialog.get_by_label('Resumed session name',exact=True)).to_have_value('Keep resume draft')
                expect(dialog.get_by_label('Existing Codex thread ID',exact=True)).to_have_value('saved-thread')
                expect(dialog.get_by_label('Resume sandbox',exact=True)).to_have_value('workspace-write')
                dialog.press('Escape')
                expect(dialog).to_have_count(0)
                page.get_by_role('button',name='+ New Session',exact=True).click()
                expect(dialog.get_by_label('Session name',exact=True)).to_have_value('Keep this draft')
                dialog.get_by_role('button',name='Close',exact=True).click()
                assert page.evaluate('document.body.scrollWidth <= innerWidth')
            page.get_by_role('button',name='Remote only',exact=False).click()
            expect(page.get_by_label('Message',exact=True)).to_have_value('Unsent conversation draft')
            assert not errors,errors
            browser.close()
        calls=[json.loads(line) for line in (root/'calls.jsonl').read_text().splitlines()]
        starts=[call for call in calls if call['method']=='thread/start']
        assert len(starts)==2 and all(len(call['params']['environments'])==1 for call in starts), starts
        assert not any(call['method']=='turn/start' for call in calls)
        daemon.terminate();daemon.wait(timeout=20)
        daemon=launch()
        api('/sessions/'+session['id']+'/connect',{})
        restored=api('/sessions/'+session['id'])
        assert restored['session']['thread_id']==session['thread_id']
        assert restored['target_selection']==detail['target_selection']
        assert restored['session']['targets'][0]['id']!=session['targets'][0]['id']
        print('PASS: New Session modal, remote-only creation, folder merging, mobile drafts, validation and runtime reconnect')
    except BaseException:
        print((root/'daemon.log').read_text()[-6000:])
        raise
    finally:
        daemon.terminate();daemon.wait(timeout=20);log.close()
