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
                elif method == 'experimentalFeature/list': result = {'data':[{'name':f'fixture_feature_{n}','stage':'experimental','enabled':False,'description':'A short feature description.'} for n in range(30)],'nextCursor':None}
                elif method in ('thread/start', 'thread/resume'):
                    result = {'thread': {'id': params.get('threadId', str(uuid.uuid4())), 'turns': []}, 'sandbox': {'type': 'dangerFullAccess' if params.get('sandbox') == 'danger-full-access' else 'readOnly'}}
                elif method == 'thread/read': result = {'thread': {'id':params['threadId'],'name':'Saved Codex title','cwd':os.environ['DEMODEX_FIXTURE_CWD'],'status': {'type': 'idle'}}}
                elif method == 'thread/goal/get': result = {'goal': None}
                elif method == 'account/rateLimits/read': result = {'rateLimits': {'limitId':'codex','primary':{'usedPercent':20,'windowDurationMins':300,'resetsAt':int(time.time())+3600},'secondary':{'usedPercent':30,'windowDurationMins':10080,'resetsAt':int(time.time())+93780}}}
                elif method == 'thread/list':
                    assert not params.get('searchTerm'), params
                    page = int(params.get('cursor') or 0)
                    if page == 1 and Path(os.environ['DEMODEX_FIXTURE_CWD'], 'delay-saved-page').exists():
                        time.sleep(1)
                    result = {'data':[{'id':'saved-thread','name':'Saved project','cwd':'/previous/project'}] if page == 0 else
                              [{'id':'aBcD-5678','name':'Late CaSeSensitive title','preview':'A searchable snippet'}] if page == 11 else
                              [{'id':f'unrelated-{page}','name':'Unrelated'}],
                              'nextCursor':str(page+1) if page < 11 else None}
                elif method == 'thread/backgroundTerminals/list': result = {'data':[{'processId':'p1','itemId':'i1','command':'sleep 60','cwd':'/remote/project'}],'nextCursor':None}
                elif method in ('thread/queue/list', 'model/list'): result = {'data': [], 'nextCursor': None}
                ws.send(json.dumps({'id': request['id'], 'result': result}))
        except ConnectionClosed: pass
    with unix_serve(handle, endpoint) as server: server.serve_forever()
    sys.exit(0)

with tempfile.TemporaryDirectory(prefix='demodex-new-session-') as temporary:
    root = Path(temporary)
    (root/'picker 🦆').mkdir()
    (root/'picker 🦆'/'child').mkdir()
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
    env = os.environ | {'PATH': str(bin_dir)+os.pathsep+os.environ['PATH'], 'DEMODEX_FIXTURE_CALLS': str(root/'calls.jsonl'), 'DEMODEX_FIXTURE_CWD': str(root)}
    log = (root/'daemon.log').open('w+')
    def launch():
        child = subprocess.Popen([os.environ.get('DEMODEX_BIN', str(ROOT/'target/rust-pwa/debug/demodex')), '--bind', f'127.0.0.1:{port}', '--data-dir', str(root/'state'), '--host-workspace', str(root), '--web-dir', str(ROOT/'web/.rust-dist')], env=env, stdout=log, stderr=log)
        for _ in range(200):
            if child.poll() is not None:
                runtime_log = root/'state/runtime/app-server.log'
                raise AssertionError((root/'daemon.log').read_text() + (runtime_log.read_text() if runtime_log.exists() else ''))
            try:
                with socket.create_connection(('127.0.0.1', port), timeout=.1): return child
            except OSError: time.sleep(.1)
        raise AssertionError('Daemon did not start')
    daemon = launch()
    token = (root/'state/access-token').read_text().strip()
    def api(path, body=None, error=None):
        from wormhole_client import api as actor_api
        return actor_api(origin, token, path, body, error)
    try:
        target = api('/targets', {'name':'Build machine','url':'ws://127.0.0.1:5012','cwd':'/remote/project'})
        target_id = target['id']
        api('/runtime/sessions', {'name':'Bad','targets':[{'id':'missing','cwd':'/project'}]}, error='Unknown target')
        api('/runtime/sessions', {'name':'Bad','targets':[{'id':target_id,'cwd':'relative'}]}, error='absolute path')
        api('/runtime/sessions', {'name':'Bad','targets':[{'id':'ssh-missing','cwd':'/project'}],'sandbox':'read-only'}, error='danger-full-access')
        assert not api('/sessions')
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(executable_path=os.environ.get('CHROME', '/run/current-system/sw/bin/google-chrome'),headless=True,args=['--no-sandbox'])
            page = browser.new_page(viewport={'width':1200,'height':850})
            errors=[]
            page.on('pageerror',lambda error:errors.append(str(error)))
            page.goto(origin)
            page.get_by_role('button', name='+ connection', exact=True).click()
            page.get_by_label('Connection name (optional)').fill('Laptop')
            page.get_by_label('Access token').fill(token)
            page.get_by_role('button',name='Save and connect',exact=True).click()
            expect(page.locator('header .current-server')).to_contain_text('Laptop')
            expect(page.locator('.weekly-usage summary')).to_contain_text('Weekly: 70% remaining')
            page.locator('.weekly-usage summary').click()
            expect(page.locator('.weekly-details')).to_contain_text('5h · 80% remaining')
            page.locator('.weekly-usage summary').click()
            page.get_by_role('button',name='Server settings',exact=True).click()
            settings=page.get_by_role('dialog',name='Server settings',exact=True)
            expect(settings.get_by_role('heading',name='Global executors',exact=True)).to_be_visible()
            host_entry=settings.locator('.target-registry .environment-card').filter(has_text='This host')
            assert host_entry.evaluate('e=>{const s=getComputedStyle(e);return [s.borderTopWidth,s.borderRightWidth,s.borderBottomWidth,s.borderLeftWidth].every(w=>parseFloat(w)>0)}')
            settings.locator('.feature-catalog > summary').click()
            expect(settings.locator('.feature-description').first).to_be_visible()
            assert settings.locator('details.feature-description').count()==0
            bounds=settings.bounding_box()
            assert bounds['height']>640 and bounds['y']>=31 and bounds['y']+bounds['height']<=819, bounds
            settings.locator('.feature-catalog > summary').click()
            page.get_by_role('button',name='+ Add SSH executor',exact=True).click()
            expect(page.get_by_label('SSH destination',exact=True)).to_be_visible()
            page.get_by_role('dialog',name='Add SSH executor',exact=True).get_by_role('button',name='Close',exact=True).click()

            expect(page.locator('main').get_by_role('button',name='Create session',exact=True)).to_have_count(0)
            page.get_by_role('button',name='+ Create VM',exact=True).click()
            expect(page.get_by_label('Environment name',exact=True)).to_be_visible()
            page.get_by_label('Memory (MiB)',exact=True).fill('12')
            assert not page.get_by_label('Memory (MiB)',exact=True).evaluate('e=>e.checkValidity()')
            page.get_by_label('Memory (MiB)',exact=True).fill('4096')
            page.get_by_role('dialog',name='New VM',exact=True).get_by_role('button',name='Close',exact=True).click()
            for width,height,label in [(1200,850,'desktop'),(390,844,'mobile')]:
                page.set_viewport_size({'width':width,'height':height})
                page.screenshot(path=str(ROOT/'target'/f'audit-settings-{label}.png'))
                page.get_by_role('button',name='+ Create container',exact=True).click()
                popup=page.get_by_role('dialog',name='New container',exact=True)
                expect(popup.get_by_label('Container name',exact=True)).to_be_visible()
                page.screenshot(path=str(ROOT/'target'/f'audit-container-{label}.png'))
                assert page.evaluate('document.body.scrollWidth <= innerWidth')
                popup.get_by_role('button',name='Close',exact=True).click()
            page.set_viewport_size({'width':1200,'height':850})
            page.get_by_role('dialog',name='Server settings',exact=True).get_by_role('button',name='Close',exact=True).click()
            page.get_by_role('button',name='+ New Session',exact=True).click()
            dialog=page.get_by_role('dialog',name='New Session',exact=True)
            dialog.get_by_label('This host · host',exact=True).click()
            expect(dialog.get_by_label('This host · host',exact=True)).to_be_checked()
            dialog.get_by_role('button',name='Browse…',exact=True).click()
            directory=page.get_by_role('dialog',name='Choose working directory',exact=True)
            entry=directory.get_by_role('button',name='picker 🦆',exact=True)
            entry.hover()
            assert entry.evaluate('e=>{const s=getComputedStyle(e);return [s.borderTopColor,s.borderRightColor,s.borderBottomColor,s.borderLeftColor].every(c=>c==="rgb(255, 121, 47)")}')
            entry.click()
            expect(directory.get_by_label('Directory path',exact=True)).to_have_value(str(root/'picker 🦆'))
            page.screenshot(path=str(ROOT/'target'/'directory-picker.png'))
            directory.get_by_role('button',name='Use this directory',exact=True).click()
            expect(dialog.get_by_label('This host (primary) working directory',exact=True)).to_have_value(str(root/'picker 🦆'))
            dialog.get_by_role('button',name='Browse…',exact=True).click()
            directory.get_by_label('Directory path',exact=True).fill(str(root/'missing'))
            directory.get_by_label('Directory path',exact=True).press('Enter')
            expect(directory.get_by_role('alert')).to_contain_text('Cannot resolve directory')
            expect(directory.get_by_role('button',name='Use this directory',exact=True)).to_be_disabled()
            page.keyboard.press('Escape')
            expect(directory).to_have_count(0)
            expect(dialog).to_be_visible()
            expect(dialog.get_by_label('This host (primary) working directory',exact=True)).to_have_value(str(root/'picker 🦆'))
            dialog.get_by_label('This host · host',exact=True).uncheck()
            dialog.get_by_label('Session name (optional)',exact=True).fill('Remote only')
            dialog.get_by_label('Build machine · external',exact=True).check()
            expect(dialog.get_by_label('This host · host',exact=True)).not_to_be_checked()
            dialog.get_by_label('Build machine (primary) working directory',exact=True).fill('/remote/project')
            dialog.get_by_label('Sandbox',exact=True).select_option('read-only')
            dialog.get_by_label('Resume existing session',exact=True).click()
            expect(dialog.get_by_label('Existing Codex thread ID',exact=True)).to_have_value('')
            expect(dialog.get_by_role('button',name='Resume session',exact=True)).to_be_disabled()
            dialog.get_by_label('Existing Codex thread ID',exact=True).fill('discard-this-draft')
            dialog.get_by_label('Resume existing session',exact=True).uncheck()
            dialog.get_by_label('Resume existing session',exact=True).check()
            expect(dialog.get_by_label('Existing Codex thread ID',exact=True)).to_have_value('')
            dialog.get_by_label('Session name (optional)',exact=True).fill('Keep resume draft')
            dialog.get_by_label('Existing Codex thread ID',exact=True).fill('saved-thread')
            dialog.get_by_label('Resume sandbox',exact=True).select_option('workspace-write')
            dialog.get_by_role('button',name='🔍 Search sessions',exact=True).click()
            picker=page.get_by_role('dialog',name='Search sessions',exact=True)
            assert picker.locator('.search-input').evaluate('e=>{const a=e.getBoundingClientRect(),b=e.querySelector("button").getBoundingClientRect();return b.right<=a.right && b.top>=a.top}')
            expect(picker.get_by_role('button',name='Saved project',exact=False)).to_be_visible()
            (root/'delay-saved-page').touch()
            picker.get_by_role('button',name='Load more',exact=True).click()
            expect(picker.get_by_role('button',name='Load more',exact=True)).to_be_disabled()
            picker.get_by_label('Search saved sessions',exact=True).fill('caseSENSITIVE')
            expect(picker.locator('.saved-thread')).to_have_count(0)
            expect(picker.get_by_role('button',name='Load more',exact=True)).to_have_count(0)
            # The older page may finish after the draft changed; it must stay discarded.
            page.wait_for_timeout(1200)
            expect(picker.locator('.saved-thread')).to_have_count(0)
            (root/'delay-saved-page').unlink()
            picker.get_by_label('Search saved sessions',exact=True).press('Enter')
            expect(picker.get_by_role('button',name='Load more',exact=True)).to_be_enabled()
            expect(picker.locator('.saved-thread')).to_have_count(0)
            picker.get_by_role('button',name='Load more',exact=True).click()
            expect(picker.get_by_role('button',name='Late CaSeSensitive title',exact=False)).to_be_visible()
            picker.get_by_label('Search saved sessions',exact=True).fill('Saved')
            picker.get_by_label('Search saved sessions',exact=True).press('Enter')
            picker.get_by_role('button',name='Saved project',exact=False).click()
            expect(picker).to_have_count(0)
            expect(dialog.get_by_label('Existing Codex thread ID',exact=True)).to_have_value('saved-thread')
            expect(dialog.get_by_label('Working directory (optional)',exact=True)).to_have_value('/previous/project')
            expect(dialog.get_by_label('Session name (optional)',exact=True)).to_have_value('Saved project')
            for width,height,label in [(1200,850,'desktop'),(390,844,'mobile')]:
                page.set_viewport_size({'width':width,'height':height})
                assert dialog.locator('.field-action').evaluate('e=>{const a=e.querySelector("input").getBoundingClientRect(),b=e.querySelector("button").getBoundingClientRect();return Math.abs(a.bottom-b.bottom)<1 && Math.abs(a.top-b.top)<1}')
                page.screenshot(path=str(ROOT/'target'/f'audit-resume-{label}.png'))
            page.set_viewport_size({'width':1200,'height':850})

            dialog.get_by_label('Session name (optional)',exact=True).fill('Keep resume draft')

            dialog.get_by_label('Resume existing session',exact=True).uncheck()
            expect(dialog.get_by_label('Session name (optional)',exact=True)).to_have_value('Remote only')
            expect(dialog.get_by_label('Sandbox',exact=True)).to_have_value('read-only')
            assert dialog.evaluate("(d) => { const ids=[...d.querySelectorAll('[id]')].map(e=>e.id); return new Set(ids).size===ids.length; }")
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
            expect(page.locator('aside .background-count').first).to_contain_text('1 background terminal running')
            expect(page.locator('.jump-latest')).to_have_count(0)
            page.get_by_role('button',name='Session controls',exact=True).click()
            rename_controls = page.get_by_role('dialog',name='Session controls',exact=True)
            old_name = rename_controls.get_by_label('Name in Demodex',exact=True).input_value()
            rename_controls.get_by_label('Name in Demodex',exact=True).fill('Renamed in controls')
            rename_controls.get_by_role('button',name='Rename session',exact=True).click()
            expect(page.locator('.session-heading h1')).to_have_text('Renamed in controls')
            rename_controls.get_by_label('Name in Demodex',exact=True).fill(old_name)
            rename_controls.get_by_role('button',name='Rename session',exact=True).click()
            expect(page.locator('.session-heading h1')).to_have_text(old_name)
            rename_controls.get_by_role('button',name='Close',exact=True).click()
            expect(page.get_by_label('Message',exact=True)).to_have_value('Unsent conversation draft')

            # A second generation at the same path must share the folder entry.
            second=api('/runtime/sessions',{'name':'Same folder','targets':[{'id':target_id,'cwd':'/remote/project'}],'sandbox':'read-only'})
            first_card=page.locator(f'[data-session-id="{session["id"]}"]')
            second_card=page.locator(f'[data-session-id="{second["id"]}"]')
            expect(second_card).to_be_visible()
            page.get_by_role('button',name='Reorder',exact=True).click()
            second_card.get_by_role('button',name='Reorder Same folder',exact=True).press('ArrowUp')
            def sibling_order():
                return first_card.locator('..').locator(':scope > .tree-agent').evaluate_all('rows=>rows.map(row=>row.dataset.sessionId)')
            expect(first_card.get_by_role('button',name='Reorder Remote only',exact=True)).to_be_enabled()
            expect(first_card.locator('..').locator(':scope > .tree-agent .session-title')).to_have_text(['Same folder','Remote only'])
            assert sibling_order()==[second['id'],session['id']]
            first_card.get_by_role('button',name='Star Remote only',exact=True).click()
            expect(first_card.get_by_role('button',name='Unstar Remote only',exact=True)).to_be_visible()
            expect(first_card.locator('..').locator(':scope > .tree-agent .session-title')).to_have_text(['Remote only','Same folder'])
            assert sibling_order()==[session['id'],second['id']]
            expect(second_card.get_by_role('button',name='Reorder Same folder',exact=True)).to_be_disabled()
            second_card.get_by_role('button',name='Star Same folder',exact=True).click()
            expect(second_card.get_by_role('button',name='Unstar Same folder',exact=True)).to_be_visible()
            expect(first_card.locator('..').locator(':scope > .tree-agent .session-title')).to_have_text(['Same folder','Remote only'])
            assert sibling_order()==[second['id'],session['id']]
            first_card.get_by_role('button',name='Reorder Remote only',exact=True).press('ArrowUp')
            expect(first_card.get_by_role('button',name='Reorder Remote only',exact=True)).to_be_enabled()
            expect(first_card.locator('..').locator(':scope > .tree-agent .session-title')).to_have_text(['Remote only','Same folder'])
            assert sibling_order()==[session['id'],second['id']]
            # Pointer drops reorder within the star partition, with one guarded operation.
            def drag_before(source, destination, touch=False, cancel=False):
                source.scroll_into_view_if_needed()
                start=source.bounding_box()
                end=destination.bounding_box()
                x,y=start['x']+start['width']/2,start['y']+start['height']/2
                tx,ty=end['x']+end['width']/2,end['y']+8
                if touch:
                    cdp=page.context.new_cdp_session(page)
                    cdp.send('Input.dispatchTouchEvent',{'type':'touchStart','touchPoints':[{'x':x,'y':y}]})
                    cdp.send('Input.dispatchTouchEvent',{'type':'touchMove','touchPoints':[{'x':tx,'y':ty}]})
                    cdp.send('Input.dispatchTouchEvent',{'type':'touchCancel' if cancel else 'touchEnd','touchPoints':[]})
                    cdp.detach()
                else:
                    page.mouse.move(x,y)
                    page.mouse.down()
                    page.mouse.move(tx,ty,steps=8)
                    if cancel: page.keyboard.press('Escape')
                    page.mouse.up()
            drag_before(second_card.get_by_role('button',name='Reorder Same folder',exact=True),first_card)
            expect(first_card.locator('..').locator(':scope > .tree-agent .session-title')).to_have_text(['Same folder','Remote only'])
            drag_before(first_card.get_by_role('button',name='Reorder Remote only',exact=True),second_card,touch=True,cancel=True)
            expect(first_card.locator('..').locator(':scope > .tree-agent .session-title')).to_have_text(['Same folder','Remote only'])
            drag_before(first_card.get_by_role('button',name='Reorder Remote only',exact=True),second_card,touch=True)
            expect(first_card.locator('..').locator(':scope > .tree-agent .session-title')).to_have_text(['Remote only','Same folder'])
            page.screenshot(path=str(ROOT/'target'/'session-reorder-desktop.png'))
            page.get_by_role('button',name='Done reordering',exact=True).click()
            expect(page.locator('.session-drag-handle')).to_have_count(0)
            second_card.get_by_role('button',name='Edit Same folder',exact=True).click()
            edit_dialog=page.get_by_role('dialog',name='Session controls',exact=True)
            expect(edit_dialog.get_by_label('Name in Demodex',exact=True)).to_have_value('Same folder')
            edit_dialog.get_by_role('button',name='Close',exact=True).click()
            page.get_by_role('button',name='Server settings',exact=True).click()
            settings=page.get_by_role('dialog',name='Server settings',exact=True)
            settings.locator('.feature-catalog > summary').click()
            settings.locator('.runtime-restart > summary').click()
            expect(settings.locator('.restart-blockers')).to_contain_text('2 background terminals running')
            expect(settings.get_by_role('button',name='Restart Codex and disconnect sessions',exact=True)).to_be_disabled()
            settings.get_by_role('button',name='Close',exact=True).click()
            expect(page.locator('aside .tree-agent')).to_have_count(2)
            expect(page.locator('aside .folder-name')).to_have_count(1)
            for width,height in [(1200,850),(390,844)]:
                page.set_viewport_size({'width':width,'height':height})
                if width==390: page.get_by_role('button',name='← Sessions',exact=True).click()
                page.get_by_role('button',name='+ New Session',exact=True).click()
                dialog.get_by_label('Session name (optional)',exact=True).fill('Keep this draft')
                dialog.get_by_label('Resume existing session',exact=True).click()
                expect(dialog.get_by_label('Session name (optional)',exact=True)).to_have_value('Keep resume draft')
                expect(dialog.get_by_label('Existing Codex thread ID',exact=True)).to_have_value('')
                expect(dialog.get_by_role('button',name='Resume session',exact=True)).to_be_disabled()
                expect(dialog.get_by_label('Resume sandbox',exact=True)).to_have_value('workspace-write')
                dialog.get_by_label('Resume existing session',exact=True).uncheck()
                dialog.press('Escape')
                expect(dialog).to_have_count(0)
                page.get_by_role('button',name='+ New Session',exact=True).click()
                expect(dialog.get_by_label('Session name (optional)',exact=True)).to_have_value('Keep this draft')
                dialog.get_by_role('button',name='Close',exact=True).click()
                assert page.evaluate('document.body.scrollWidth <= innerWidth')
            page.locator('.session').filter(has_text='Remote only').click()
            expect(page.get_by_label('Message',exact=True)).to_have_value('Unsent conversation draft')
            page.set_viewport_size({'width':1200,'height':850})
            page.get_by_role('button',name='+ New Session',exact=True).click()
            dialog.get_by_label('Session name (optional)',exact=True).fill('')
            expect(dialog.get_by_role('button',name='Create session',exact=True)).to_be_enabled()
            dialog.get_by_role('button',name='Create session',exact=True).click()
            empty=next(s for s in api('/sessions') if s['name']=='Untitled session')
            assert empty['targets']==[] and api('/sessions/'+empty['id'])['target_selection']==[]
            # Staged SSH is private, starts only after Create, and failed
            # attachment leaves exactly one session plus an editable retry form.
            page.get_by_role('button',name='+ New Session',exact=True).click()
            dialog.get_by_label('Session name (optional)',exact=True).fill('SSH draft')
            dialog.get_by_role('button',name='+ Add SSH',exact=True).click()
            ssh_editor=page.get_by_role('dialog',name='Add SSH',exact=True)
            ssh_editor.get_by_label('SSH executor name',exact=True).fill('Test SSH')
            ssh_editor.get_by_label('SSH destination',exact=True).fill('-invalid')
            ssh_editor.get_by_label('Remote working directory',exact=True).fill('/workspace')
            ssh_editor.get_by_role('button',name='Add to session',exact=True).click()
            expect(dialog.get_by_role('button',name='Create session',exact=True)).to_be_disabled()
            dialog.get_by_label('Sandbox',exact=True).select_option('danger-full-access')
            dialog.get_by_role('button',name='Create session',exact=True).click()
            controls=page.get_by_role('dialog',name='Session controls',exact=True)
            expect(controls).to_be_visible()
            expect(controls.get_by_role('alert')).to_be_visible()
            controls.get_by_role('button',name='+ Add SSH executor to this session',exact=True).click()
            ssh_popup=page.get_by_role('dialog',name='Add session SSH executor',exact=True)
            expect(ssh_popup.get_by_label('SSH destination',exact=True)).to_have_value('-invalid')
            ssh_popup.get_by_role('button',name='Close',exact=True).click()
            assert len([s for s in api('/sessions') if s['name']=='SSH draft'])==1
            for width,height,label in [(1200,850,'desktop'),(390,844,'mobile')]:
                page.set_viewport_size({'width':width,'height':height})
                page.screenshot(path=str(ROOT/'target'/f'controls-{label}.png'))
                assert page.evaluate('document.body.scrollWidth <= innerWidth')
            controls.get_by_role('button',name='Close',exact=True).click()
            page.set_viewport_size({'width':1200,'height':850})
            card = page.locator('.tree-agent').first
            geometry = card.evaluate("""e => {
                const main=e.querySelector('.session'), corner=e.querySelector('.session-archive-action');
                const a=main.getBoundingClientRect(), b=corner.getBoundingClientRect();
                const slots=e.querySelector('.session-activity-slots'), saved=slots.innerHTML;
                const height=a.height;
                slots.innerHTML='<span><small>Goal · active</small></span><span><small>999 background terminals running</small></span><span><small>999 active subagents</small></span>';
                const filled=main.getBoundingClientRect().height;
                slots.innerHTML='<span></span><span></span>';
                const empty=main.getBoundingClientRect().height;
                slots.innerHTML=saved;
                return {height,filled,empty,top:a.top-b.top,right:a.right-b.right};
            }""")
            assert geometry['height']==geometry['filled']==geometry['empty'], geometry
            assert abs(geometry['top'])<1 and abs(geometry['right'])<1, geometry
            page.screenshot(path=str(ROOT/'target'/'session-overview.png'))
            assert not errors,errors
            imported_id = str(uuid.uuid4())
            imported = api('/host/sessions', {'name':'  ','thread_id':imported_id,'cwd':str(root)})
            assert imported['name'] == 'Saved Codex title', imported
            from wormhole_client import call
            found = call(origin,token,{'SavedThreads':{'cursor':None,'search':imported_id}})
            assert found['data'][0]['id'] == imported_id, found
            def search(query, cursor=None):
                return call(origin,token,{'SavedThreads':{'cursor':cursor,'search':query}})
            assert search('  AVED PROJ  ')['data'][0]['id'] == 'saved-thread'
            for query in ('sEsEnSi', 'BCD-56', 'CHABLE SNIP'):
                first = search(query)
                assert first['data'] == [] and first['nextCursor'] == '10', first
                second_page = search(query, first['nextCursor'])
                assert second_page['data'][0]['id'] == 'aBcD-5678' and second_page['nextCursor'] is None, second_page
            first = search('does not exist')
            assert search('does not exist', first['nextCursor']) == {'data':[], 'nextCursor':None}
            browser.close()
        calls=[json.loads(line) for line in (root/'calls.jsonl').read_text().splitlines()]
        starts=[call for call in calls if call['method']=='thread/start']
        assert len(starts)==4 and sorted(len(call['params']['environments']) for call in starts)==[0,0,1,1], starts
        assert not any(call['method']=='turn/start' for call in calls)
        daemon.terminate();daemon.wait(timeout=20)
        daemon=launch()
        api('/sessions/'+session['id']+'/connect',{})
        restored=api('/sessions/'+session['id'])
        assert restored['session']['thread_id']==session['thread_id']
        assert restored['session']['starred']
        assert restored['session']['sort_order'] < api('/sessions/'+second['id'])['session']['sort_order']
        assert restored['target_selection']==detail['target_selection']
        assert restored['session']['targets'][0]['id']!=session['targets'][0]['id']
        print('PASS: New Session modal, remote-only creation, folder merging, mobile drafts, validation and runtime reconnect')
    except BaseException:
        try: page.screenshot(path='/tmp/demodex-picker-failure.png')
        except Exception: pass
        print((root/'daemon.log').read_text()[-6000:])
        raise
    finally:
        daemon.terminate();daemon.wait(timeout=20);log.close()
