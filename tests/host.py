# /// script
# dependencies = ["websockets>=15", "playwright"]
# ///
"""Exercise native host mode, persistence and owned-process cleanup without inference."""
import base64
import json
import os
import re
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

    external_executor = None
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
        controls=api('/sessions/'+session['id'])['controls']
        catalog=api('/sessions/'+session['id']+'/models')['data']
        assert catalog, 'real Codex returned an empty model catalog'
        available=next((m for m in catalog if m['model']==controls['settings']['effective']['model']),catalog[0])
        efforts=[e['reasoningEffort'] for e in available['supportedReasoningEfforts']]
        effort=next((e for e in efforts if e!=controls['settings']['effective'].get('effort')),efforts[0])
        selection={'model':available['model'],'effort':effort,'serviceTier':None}
        accepted=api('/sessions/'+session['id']+'/model',selection)
        assert accepted['model']==selection['model'] and accepted['effort']==effort,accepted
        assert accepted['serviceTier'] in (None,'default'),accepted
        selection=accepted
        assert api('/sessions/'+session['id'])['controls']['settings']['effective']==selection
        # No inference: real goal testing only creates a paused objective.
        goal_supported=controls['goalError'] is None
        if goal_supported:
            goal=api('/sessions/'+session['id']+'/goal',{'action':'save','objective':'Disposable paused control fixture','tokenBudget':1000})['goal']
            assert goal['status']=='paused' and goal['tokenBudget']==1000,goal
        for mode,kind in [('danger-full-access','dangerFullAccess'),('workspace-write','workspaceWrite'),('workspace-write','workspaceWrite')]:
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
        # Register a real second executor and select it without sending a model turn.
        with socket.socket() as sock:
            sock.bind(('127.0.0.1',0))
            extra_port=sock.getsockname()[1]
        extra_profile=root/'executor-profile'
        extra_profile.mkdir()
        extra_env=dict(os.environ,CODEX_HOME=str(extra_profile))
        external_executor=subprocess.Popen(['codex','exec-server','--listen',f'ws://127.0.0.1:{extra_port}'],env=extra_env,stdout=log,stderr=log)
        for _ in range(100):
            try:
                with socket.create_connection(('127.0.0.1',extra_port),timeout=.1): break
            except OSError: time.sleep(.1)
        registered=api('/targets',{'name':'Shared secondary','url':f'ws://127.0.0.1:{extra_port}','cwd':str(workspace)})
        selected_targets=[{'id':'host','cwd':str(chosen_workspace)},{'id':registered['id'],'cwd':str(workspace)}]
        api('/sessions/'+session['id']+'/targets',{'targets':selected_targets})
        selected_detail=api('/sessions/'+session['id'])
        assert selected_detail['target_selection']==selected_targets,selected_detail
        assert selected_detail['targets_pending'] is True
        assert len(selected_detail['session']['targets'])==2
        assert selected_detail['session']['thread_id']==session['thread_id']
        try:
            api('/targets/'+registered['id']+'/forget',{})
            raise AssertionError('attached target forgotten')
        except AssertionError as error:
            assert 'Detach this target' in str(error),error
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
        assert len(resumed['targets'])==2,resumed
        assert api('/sessions/'+session['id'])['target_selection']==selected_targets
        assert next(t for t in api('/targets') if t['id']==registered['id'])['users']==[session['id']]
        api('/sessions/'+session['id']+'/targets',{'targets':selected_targets[:1]})
        api('/targets/'+registered['id']+'/forget',{})
        assert not any(t['id']==registered['id'] for t in api('/targets'))
        print('PASS: real multi-target registration, paused selection, durable reconnect and detach/forget without inference')
        resumed_controls=api('/sessions/'+session['id'])['controls']
        assert resumed_controls['settings']['effective']==selection,resumed_controls
        if goal_supported:
            assert resumed_controls['goal']['status']=='paused',resumed_controls
            assert resumed_controls['goal']['objective']=='Disposable paused control fixture'
            api('/sessions/'+session['id']+'/goal',{'action':'clear'})
            assert api('/sessions/'+session['id'])['controls']['goal'] is None
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
            # Upload through the real browser transport, without submitting a prompt.
            page.get_by_role('button', name='Host persistence test', exact=False).click()
            page.get_by_role('button',name='Session controls',exact=True).click()
            page.locator('.session-settings summary').click()
            sandbox = page.get_by_label('Session sandbox', exact=True)
            expect(sandbox).to_have_value('workspace-write')
            sandbox.select_option('danger-full-access')
            expect(page.locator('.sandbox-pending')).to_be_visible()
            expect(page.locator('.runtime-panel strong')).to_have_text('Workspace-write')
            page.get_by_role('button',name='Apply sandbox',exact=True).click()
            expect(page.locator('.runtime-panel strong')).to_have_text('Danger-full-access',timeout=20000)
            expect(page.locator('.sandbox-pending')).to_have_count(0)
            page.reload()
            page.get_by_role('button',name='Session controls',exact=True).click()
            page.locator('.session-settings summary').click()
            expect(sandbox).to_have_value('danger-full-access',timeout=20000)
            # A different client changes the policy while this select has an unsaved choice.
            sandbox.select_option('workspace-write')
            expect(page.locator('.sandbox-pending')).to_be_visible()
            api('/sessions/'+session['id']+'/sandbox',{'sandbox':'read-only'})
            expect(sandbox).to_have_value('read-only',timeout=20000)
            expect(page.locator('.runtime-panel strong')).to_have_text('Read-only')
            sandbox.select_option('')
            page.get_by_role('button',name='Apply sandbox',exact=True).click()
            expect(sandbox).to_have_value('')
            expect(page.locator('.sandbox-pending')).to_have_count(0,timeout=20000)
            assert api('/sessions/'+session['id'])['session']['effective_sandbox']['type']=='readOnly'
            sandbox.select_option('workspace-write')
            page.get_by_role('button',name='Apply sandbox',exact=True).click()
            expect(page.locator('.runtime-panel strong')).to_have_text('Workspace-write',timeout=20000)
            page.get_by_role('button',name='Close',exact=True).click()
            prompt = page.get_by_label('Message', exact=True)
            prompt.fill('🙂 replace end')
            prompt.evaluate('(el) => el.setSelectionRange(3, 10)')
            png = base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=')
            with page.expect_file_chooser() as chooser:
                page.get_by_role('button', name='Attach image', exact=True).click()
            chooser.value.set_files({'name': '../../screenshot.png', 'mimeType': 'image/png', 'buffer': png})
            expect(prompt).to_have_value(re.compile(r'🙂 \"/.*/uploads/[^/]+\.png\" end'), timeout=20000)
            image_path = Path(prompt.input_value().split('"')[1])
            assert image_path.read_bytes() == png
            assert image_path.parent == data / 'uploads'
            assert image_path.stat().st_mode & 0o777 == 0o600
            draft = prompt.input_value()
            page.reload()
            expect(prompt).to_have_value(draft, timeout=20000)
            # Clipboard uploads append at the current selection and remain unsent.
            prompt.evaluate('(el) => el.setSelectionRange(el.value.length, el.value.length)')
            prompt.evaluate("""(el, data) => {
                const clipboardData = new DataTransfer();
                clipboardData.items.add(new File([new Uint8Array(data)], 'paste.png', {type:'image/png'}));
                el.dispatchEvent(new ClipboardEvent('paste', {clipboardData, bubbles:true, cancelable:true}));
            }""", list(png))
            expect(page.get_by_role('button', name='Attach image', exact=True)).to_be_enabled(timeout=20000)
            expect(prompt).to_have_value(re.compile(r' end \"/.*/uploads/[^/]+\.png\"$'), timeout=20000)
            assert len(list((data/'uploads').iterdir())) == 2
            page.get_by_label('Upload image', exact=True).set_input_files({'name':'bad.png','mimeType':'image/png','buffer':b'not an image'})
            expect(page.get_by_role('alert')).to_contain_text('Choose a PNG', timeout=20000)
            assert len(list((data/'uploads').iterdir())) == 2
            assert not any(e['message'].get('method') == 'demodex/promptAccepted' for e in api('/sessions/'+session['id']+'/events'))
            page.get_by_role('button',name='+ New Session',exact=True).click()
            page.get_by_text('Resume a saved session',exact=True).click()
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
            expect(page.get_by_label('Existing Codex thread ID', exact=True)).to_have_value(session['thread_id'])
            expect(page.get_by_label('Working directory (optional)')).to_have_value('')
            browser.close()
        print('PASS: image picker and clipboard uploads, private persistent files, draft recovery, invalid-image rejection and no prompt submission')
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
        if external_executor is not None:
            external_executor.terminate()
            external_executor.wait(timeout=15)
        log.close()
