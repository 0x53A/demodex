# /// script
# dependencies = ["websockets>=15", "playwright"]
# ///
"""Exercise native host mode, persistence and owned-process cleanup without inference."""
import base64
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from websockets.sync.client import connect, unix_connect
from playwright.sync_api import sync_playwright, expect

ROOT = Path(__file__).resolve().parents[1]


def rpc(ws, method, params, ident=1):
    ws.send(json.dumps({'id': ident, 'method': method, 'params': params}))
    while True:
        message = json.loads(ws.recv(timeout=20))
        if message.get('id') == ident:
            assert 'error' not in message, message
            return message['result']


with tempfile.TemporaryDirectory(prefix='demodex-host-') as temporary:
    root = Path(temporary)
    workspace = root / 'workspace'
    workspace.mkdir()
    chosen_workspace = root / 'selected-project'
    chosen_workspace.mkdir()
    data = root / 'state'
    profile_args = []
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    log = (root / 'manager.log').open('w+')

    def launch():
        child = subprocess.Popen([os.environ.get('DEMODEX_BIN', str(ROOT / 'target/rust-pwa/debug/demodex')), '--data-dir', str(data),
            '--bind', f'127.0.0.1:{port}', '--host-workspace', str(workspace), *profile_args], stdout=log, stderr=log)
        for _ in range(200):
            assert child.poll() is None, 'manager exited'
            try:
                with socket.create_connection(('127.0.0.1', port), timeout=.1):
                    return child
            except OSError:
                time.sleep(.1)
        raise AssertionError('manager not ready')

    manager = launch()
    token = (data / 'access-token').read_text().strip()

    def api(path, body=None):
        request = urllib.request.Request(f'http://127.0.0.1:{port}/api{path}',
            data=None if body is None else json.dumps(body).encode(),
            headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            raise AssertionError(error.read().decode()) from error

    try:
        runtime = api('/runtime')
        assert runtime['running'] and runtime['mode'] == 'host'
        assert runtime['account'] is None
        assert not (data / 'runtime/home/auth.json').exists()
        for invalid_cwd in ('relative/path', str(root/'missing-directory')):
            try:
                api('/host/sessions', {'name':'Invalid directory','cwd':invalid_cwd})
                raise AssertionError('invalid directory accepted')
            except AssertionError as error:
                assert 'existing absolute directory' in str(error),error
        assert api('/sessions')==[], 'invalid directories must not create sessions'
        session = api('/host/sessions', {'name': 'Host persistence test', 'sandbox':'read-only','cwd':str(chosen_workspace)})
        assert session['thread_id'], session
        assert session['targets'][0]['cwd']==str(chosen_workspace),session
        assert session['sandbox']=='read-only',session
        assert session['effective_sandbox']['type']=='readOnly',session
        for mode,kind in [('danger-full-access','dangerFullAccess'),('workspace-write','workspaceWrite')]:
            api('/sessions/'+session['id']+'/sandbox',{'sandbox':mode})
            session=api('/sessions/'+session['id'])['session']
            assert session['sandbox']==mode and session['effective_sandbox']['type']==kind,session
        with unix_connect(session['endpoint'].removeprefix('unix://'), compression=None) as ws:
            rpc(ws, 'initialize', {'clientInfo': {'name': 'host_test', 'version': '0'}, 'capabilities': {'experimentalApi': True}})
            ws.send(json.dumps({'method': 'initialized', 'params': {}}))
            rpc(ws, 'thread/inject_items', {'threadId': session['thread_id'], 'items': [
                {'type': 'message', 'role': 'user', 'content': [{'type': 'input_text', 'text': 'No-model host fixture.'}]}]}, 2)
            snapshot=rpc(ws,'thread/read',{'threadId':session['thread_id'],'includeTurns':False},3)
            assert snapshot['thread']['cwd']==str(chosen_workspace),snapshot
        target = session['targets'][0]
        marker = chosen_workspace / 'executor-proof'
        with connect(target['url'], compression=None) as ws:
            rpc(ws, 'initialize', {'clientName': 'fixture'})
            ws.send(json.dumps({'method': 'initialized', 'params': {}}))
            rpc(ws, 'fs/writeFile', {'path': marker.as_uri(), 'dataBase64': base64.b64encode(b'host executor').decode()}, 2)
        assert marker.read_text() == 'host executor'
        manager.terminate()
        manager.wait(timeout=30)
        # inject_items persists response items but intentionally doesn't create
        # Codex's first-user-message metadata used by thread/list. Add a user
        # event to this disposable rollout while its owning process is stopped.
        rollouts=list((data/'runtime/home/sessions').rglob('*'+session['thread_id']+'*.jsonl'))
        assert len(rollouts)==1, rollouts
        last=json.loads(rollouts[0].read_text().splitlines()[-1])
        with rollouts[0].open('a') as rollout:
            rollout.write(json.dumps({'timestamp':last['timestamp'],'ordinal':last.get('ordinal',0)+1,'type':'event_msg','payload':{'type':'user_message','message':'No-model host fixture.','images':[],'local_images':[],'text_elements':[]}})+'\n')
        assert not (data / 'runtime/ipc/app.sock').exists()
        executor_port = int(target['url'].rsplit(':', 1)[1])
        with socket.socket() as sock:
            assert sock.connect_ex(('127.0.0.1', executor_port)) != 0, 'orphaned executor'
        manager = launch()
        api('/sessions/' + session['id'] + '/connect', {})
        resumed = api('/sessions/' + session['id'])['session']
        assert resumed['thread_id'] == session['thread_id']
        assert resumed['sandbox']=='workspace-write' and resumed['effective_sandbox']['type']=='workspaceWrite',resumed
        assert resumed['targets'][0]['id'] != target['id']
        assert resumed['targets'][0]['cwd'] == str(chosen_workspace)
        assert marker.read_text() == 'host executor'
        # Discover the real persisted thread through the PWA's Wormhole API,
        # without inference, shared credentials, or creating another attachment.
        with unix_connect(resumed['endpoint'].removeprefix('unix://'), compression=None) as indexer:
            rpc(indexer, 'initialize', {'clientInfo': {'name': 'host_test', 'version': '0'}, 'capabilities': {'experimentalApi': True}})
            indexer.send(json.dumps({'method':'initialized','params':{}}))
            for attempt in range(20):
                listed=rpc(indexer,'thread/list',{'limit':50,'sortKey':'updated_at','modelProviders':[]},attempt+2)
                if any(t['id']==session['thread_id'] for t in listed['data']):
                    break
                time.sleep(.2)
            else:
                raise AssertionError(('fixture was not indexed',listed))
        with sync_playwright() as playwright:
            browser=playwright.chromium.launch(executable_path='/run/current-system/sw/bin/google-chrome',headless=True,args=['--no-sandbox'])
            page=browser.new_page()
            page.goto(f'http://127.0.0.1:{port}')
            page.get_by_label('Access token').fill(token)
            page.get_by_role('button',name='Connect host',exact=True).click()
            expect(page.locator('header .indicator')).to_have_text('CONNECTED',timeout=20000)
            page.get_by_role('button',name='Environments',exact=True).click()
            page.get_by_role('button',name='Find saved sessions').click()
            try:
                expect(page.locator('.saved-threads .session').filter(has_text=session['thread_id'])).to_be_visible(timeout=10000)
            except AssertionError:
                print('Saved-thread UI:',page.locator('main').inner_text())
                with unix_connect(session['endpoint'].removeprefix('unix://'), compression=None) as diagnostic:
                    rpc(diagnostic, 'initialize', {'clientInfo': {'name': 'host_test', 'version': '0'}, 'capabilities': {'experimentalApi': True}})
                    diagnostic.send(json.dumps({'method': 'initialized', 'params': {}}))
                    print('Thread list:',rpc(diagnostic,'thread/list',{'limit':50,'modelProviders':[]},2))
                raise
            page.locator('.saved-threads .session').filter(has_text=session['thread_id']).click()
            expect(page.get_by_label('Existing Codex thread ID (optional)')).to_have_value(session['thread_id'])
            expect(page.get_by_label('Working directory (optional)')).to_have_value('')
            browser.close()
        print('PASS: saved-thread discovery through Wormhole; native host runtime, empty dedicated login, real host executor, durable thread resume, fresh target and cleanup')
        manager.terminate()
        manager.wait(timeout=30)
        profile = data / 'runtime/home'
        config_before = (profile / 'config.toml').read_bytes()
        original_workspace = chosen_workspace
        workspace = root / 'other-workspace'
        workspace.mkdir()
        profile_args = ['--codex-home', str(profile)]
        data = root / 'new-manager-state'
        manager = launch()
        token = (data / 'access-token').read_text().strip()
        assert api('/runtime')['profile'] == 'inherited'
        imported = api('/host/sessions', {'name': 'Imported session', 'thread_id': session['thread_id'], 'sandbox':'danger-full-access'})
        assert imported['thread_id'] == session['thread_id']
        assert imported['status'] == 'connected', imported
        assert imported['effective_sandbox']['type']=='dangerFullAccess',imported
        assert imported['targets'][0]['cwd'] == str(original_workspace)
        duplicate = api('/host/sessions', {'name': 'Same thread', 'thread_id': session['thread_id'], 'sandbox':'danger-full-access'})
        assert duplicate['id'] == imported['id']
        try:
            api('/host/sessions',{'name':'Retarget existing','thread_id':session['thread_id'],'cwd':str(workspace)})
            raise AssertionError('existing attachment unexpectedly retargeted')
        except AssertionError as error:
            assert 'different working directory' in str(error),error
        assert (profile / 'config.toml').read_bytes() == config_before
        assert not (data / 'runtime/home').exists()
        print('PASS: shared profile imports existing thread/history and original working directory, preserves config and avoids duplicate attachment')
        manager.terminate()
        manager.wait(timeout=30)
        data=root/'override-state'
        manager=launch()
        token=(data/'access-token').read_text().strip()
        imported=api('/host/sessions',{'name':'Explicit resume directory','thread_id':session['thread_id'],'cwd':str(workspace)})
        assert imported['status']=='connected',imported
        assert imported['targets'][0]['cwd']==str(workspace),imported
        with unix_connect(imported['endpoint'].removeprefix('unix://'),compression=None) as ws:
            rpc(ws,'initialize',{'clientInfo':{'name':'cwd_test','version':'0'},'capabilities':{'experimentalApi':True}})
            ws.send(json.dumps({'method':'initialized','params':{}}))
            snapshot=rpc(ws,'thread/read',{'threadId':session['thread_id'],'includeTurns':False},2)
            # Codex's top-level cwd is historical metadata on resumed threads;
            # the live execution environments carry the selected working directory.
            active=[e for e in snapshot['thread']['environments'] if e['environmentId']==imported['targets'][0]['id']]
            assert active and active[0]['cwd']==str(workspace),snapshot
        manager.terminate()
        manager.wait(timeout=30)
        manager=launch()
        api('/sessions/'+imported['id']+'/connect',{})
        assert api('/sessions/'+imported['id'])['session']['targets'][0]['cwd']==str(workspace)
        print('PASS: custom new-session directory persists; blank resume preserves it; explicit resume override updates thread and executor consistently')
    except BaseException:
        for path in root.rglob('*.log'):
            print(path, path.read_text(errors='replace')[-3000:])
        raise
    finally:
        manager.terminate()
        manager.wait(timeout=30)
        log.close()
