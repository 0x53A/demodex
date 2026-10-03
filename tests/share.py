# /// script
# dependencies = ["playwright", "websockets>=15"]
# ///
"""System share POST, drafts, original files and receipt recovery; no inference."""
import functools
import http.server
import json
import os
from pathlib import Path
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
import urllib.error
from playwright.sync_api import sync_playwright, expect

ROOT = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='demodex-share-') as temporary:
    root = Path(temporary)
    (root/'no-background').touch()
    bin_dir = root/'bin'
    bin_dir.mkdir()
    real_codex = shutil.which('codex')
    assert real_codex
    wrapper = bin_dir/'codex'
    wrapper.write_text('#!/bin/sh\nif [ "$1" = app-server ]; then\n shift\n exec '+shlex.quote(sys.executable)+' '+shlex.quote(str(ROOT/'tests/new_session.py'))+' --fixture "$@"\nfi\nexec '+shlex.quote(real_codex)+' "$@"\n')
    wrapper.chmod(0o700)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
    origin=f'http://127.0.0.1:{port}'
    env=os.environ|{'PATH':str(bin_dir)+os.pathsep+os.environ['PATH'],'DEMODEX_FIXTURE_CALLS':str(root/'calls.jsonl'),'DEMODEX_FIXTURE_CWD':str(root)}
    log=(root/'daemon.log').open('w+')
    daemon=subprocess.Popen([os.environ.get('DEMODEX_BIN',str(ROOT/'target/rust-pwa/debug/demodex')),'--bind',f'127.0.0.1:{port}','--data-dir',str(root/'state'),'--host-workspace',str(root),'--web-dir',str(ROOT/'web/.rust-dist')],env=env,stdout=log,stderr=log)
    try:
        for _ in range(200):
            if daemon.poll() is not None: raise AssertionError((root/'daemon.log').read_text())
            try:
                with socket.create_connection(('127.0.0.1',port),timeout=.1): break
            except OSError: time.sleep(.1)
        token=(root/'state/access-token').read_text().strip()
        from wormhole_client import api as actor_api, call as actor_call
        def api(path,body=None): return actor_api(origin,token,path,body)
        try:
            actor_call(origin,'invalid-token',{'UploadFile':{'id':'none','name':'unauthorized.heic','data':'YWJj'}})
            raise AssertionError('Unauthenticated file upload was accepted')
        except AssertionError as error:
            assert 'Access token rejected' in str(error),str(error)
        assert not (root/'state/uploads').exists()
        existing=api('/runtime/sessions',{'name':'Existing agent','targets':[{'id':'host','cwd':str(root)}]})
        archived=api('/runtime/sessions',{'name':'Archived agent','targets':[{'id':'host','cwd':str(root)}]})
        api(f'/sessions/{archived["id"]}/archive',{'archived':True})
        # The daemon never accepts an unauthenticated share-target HTTP upload.
        try:
            urllib.request.urlopen(urllib.request.Request(origin+'/share-target',data=b'private photo')).read()
            raise AssertionError('Public upload endpoint accepted a request')
        except urllib.error.HTTPError as error: assert error.code in (404,405), error
        with sync_playwright() as p:
            browser=p.chromium.launch(executable_path=os.environ.get('CHROME','/run/current-system/sw/bin/google-chrome'),headless=True,args=['--no-sandbox'])
            context=browser.new_context(viewport={'width':390,'height':844})
            page=context.new_page()
            errors=[]
            page.on('pageerror',lambda e:errors.append(str(e)))
            page.goto(origin)
            page.get_by_role('button',name='+ connection',exact=True).click()
            page.get_by_label('Access token').fill(token)
            page.get_by_role('button',name='Save and connect',exact=True).click()
            expect(page.locator('header .current-server')).to_contain_text('connected to')
            page.evaluate('navigator.serviceWorker.ready')
            page.wait_for_function('navigator.serviceWorker.controller !== null')
            page.locator('.session-open').first.click() if page.locator('.session-open').count() else None
            def share(fields,files=[]):
                with page.expect_navigation():
                    page.evaluate('''({fields,files})=>{
                      const form=document.createElement('form'); form.method='POST'; form.enctype='multipart/form-data'; form.action=new URL('./share-target',document.baseURI);
                      for(const [name,value] of Object.entries(fields)){const input=document.createElement('textarea');input.name=name;input.value=value;form.append(input);}
                      if(files.length){const input=document.createElement('input');input.type='file';input.name='files';input.multiple=true;const transfer=new DataTransfer();
                        for(const file of files)transfer.items.add(new File([new Uint8Array(file.size).fill(file.byte)],file.name,{type:file.type||''}));
                        input.files=transfer.files;form.append(input);}
                      document.body.append(form); form.submit();
                    }''',{'fields':fields,'files':files})
                expect(page.get_by_role('dialog',name='Shared content',exact=True)).to_be_visible()
                return page.get_by_role('dialog',name='Shared content',exact=True)
            # Local receiving survives an offline launch and reload without submitting anything.
            context.set_offline(True)
            picker=share({'text':'  exact shared text\nλ  ','url':'https://example.test/raw?x=1'})
            expect(picker).to_contain_text('Connect to a server')
            page.reload()
            expect(page.get_by_role('dialog',name='Shared content',exact=True)).to_be_visible()
            context.set_offline(False)
            page.evaluate("window.dispatchEvent(new Event('online'))")
            picker=page.get_by_role('dialog',name='Shared content',exact=True)
            choice=picker.get_by_role('button').filter(has_text='Existing agent')
            expect(choice).to_be_enabled(timeout=20000)
            expect(picker).not_to_contain_text('Archived agent')
            choice.click()
            expect(page.get_by_label('Message',exact=True)).to_have_value('  exact shared text\nλ  \nhttps://example.test/raw?x=1')
            page.get_by_label('Message',exact=True).fill('Existing draft 🦆')
            # Multiple files: arbitrary original bytes and filenames, including >4 MiB HEIC.
            picker=share({'text':'photo context'},[{'name':"original's 🦆.HEIC",'size':5*1024*1024,'byte':171,'type':'image/heic'},{'name':'notes.txt','size':17,'byte':90,'type':'text/plain'}])
            expect(picker).to_contain_text('Large files may take a while')
            picker.get_by_role('button').filter(has_text='Existing agent').click()
            expect(page.get_by_role('dialog',name='Shared content',exact=True)).to_have_count(0,timeout=60000)
            value=page.get_by_label('Message',exact=True).input_value()
            assert value.startswith('Existing draft 🦆\nphoto context\n'), value
            paths=[json.loads(line) for line in value.splitlines()[2:]]
            assert Path(paths[0]).name=="original's 🦆.HEIC"
            assert Path(paths[0]).read_bytes()==bytes([171])*(5*1024*1024)
            assert Path(paths[1]).read_bytes()==b'Z'*17
            page.reload()
            expect(page.get_by_label('Message',exact=True)).to_have_value(value)
            assert 'share=' not in page.url
            # New-session flow preserves the share across cancel/reopen, then fills its draft.
            picker=share({'text':'new agent task'})
            picker.get_by_role('button',name='New session',exact=True).click()
            creation=page.get_by_role('dialog',name='New Session',exact=True)
            creation.get_by_role('button',name='Close',exact=True).click()
            expect(picker).to_be_visible()
            picker.get_by_role('button',name='New session',exact=True).click()
            creation.get_by_label('Session name (optional)',exact=True).fill('Shared new agent')
            creation.get_by_role('button',name='Create session',exact=True).click()
            expect(page.get_by_label('Message',exact=True)).to_have_value('new agent task',timeout=20000)
            # A journal left mid-upload must not restart the write on reload.
            picker=share({'text':'recover me'},[{'name':'interrupted.heic','size':19,'byte':17}])
            page.evaluate('''async session=>{await demodexShares.begin(demodexShares.route(),location.origin,session)}''',existing['id'])
            before=list((root/'state/uploads').rglob('*'))
            page.reload()
            picker=page.get_by_role('dialog',name='Shared content',exact=True)
            expect(picker).to_contain_text('unconfirmed · receipt')
            assert list((root/'state/uploads').rglob('*'))==before
            picker.get_by_role('button',name='Discard share',exact=True).click()
            expect(picker).to_have_count(0)
            calls=[json.loads(line) for line in (root/'calls.jsonl').read_text().splitlines()]
            assert not any(c['method']=='turn/start' for c in calls),'Sharing submitted a model turn'
            assert not errors,errors
            # Static subpath installation: the worker handles POST without any server handler.
            static=root/'static'; (static/'demodex').mkdir(parents=True)
            shutil.copytree(ROOT/'web/.share-subpath-dist',static/'demodex',dirs_exist_ok=True)
            class Quiet(http.server.SimpleHTTPRequestHandler):
                def log_message(self,*_): pass
            server=http.server.ThreadingHTTPServer(('127.0.0.1',0),functools.partial(Quiet,directory=str(static)))
            threading.Thread(target=server.serve_forever,daemon=True).start()
            try:
                sub=browser.new_page();sub.goto(f'http://127.0.0.1:{server.server_port}/demodex/')
                sub.evaluate('navigator.serviceWorker.ready');sub.wait_for_function('navigator.serviceWorker.controller !== null')
                # Explicitly handle the initial connection picker first.
                sub.get_by_role('dialog',name='Connections',exact=True).get_by_role('button',name='Close',exact=True).click()
                with sub.expect_navigation():
                    sub.evaluate('''()=>{let f=document.createElement('form');f.method='POST';f.enctype='multipart/form-data';f.action='./share-target';let i=document.createElement('input');i.name='text';i.value='subpath';f.append(i);document.body.append(f);f.submit()}''')
                expect(sub.get_by_role('dialog',name='Shared content',exact=True)).to_contain_text('subpath')
                assert '/demodex/?share=' in sub.url,sub.url
            finally: server.shutdown()
            browser.close()
        print('Share target: text, offline/reload, existing/new sessions, original files, large-file warning, no replay and subpath passed')
    finally:
        daemon.terminate()
        try:daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:daemon.kill();daemon.wait()
        log.close()
