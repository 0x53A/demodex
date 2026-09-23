# /// script
# dependencies = ["websockets>=15", "playwright"]
# ///
"""Disposable OpenSSH server + local adapter; no model calls or existing credentials."""
import base64
import getpass
import json
import os
import re
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import sys
import time
import urllib.error
import urllib.request
from playwright.sync_api import sync_playwright, expect
from websockets.sync.client import connect
from websockets.sync.server import serve

ROOT = Path(__file__).resolve().parents[1]

def port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def wait_port(number):
    for _ in range(150):
        try:
            with socket.create_connection(('127.0.0.1', number), timeout=.1): return
        except OSError: time.sleep(.1)
    raise AssertionError(f'Port {number} unavailable')


class Client:
    def __init__(self, url):
        self.ws = connect(url,legacy=True)
        self.serial = 0
        self.events = []
    def call(self, method, params=None, error=None):
        self.serial += 1
        self.ws.send(json.dumps({'jsonrpc':'2.0','id':self.serial,'method':method,'params':params or {}}))
        while True:
            value = json.loads(self.ws.recv(timeout=60))
            if 'id' not in value:
                self.events.append(value)
                continue
            assert value['id'] == self.serial, value
            if error:
                assert error.lower() in value['error']['message'].lower(), value
                return value
            assert 'error' not in value, value
            return value['result']
    def close(self): self.ws.close()


with tempfile.TemporaryDirectory(prefix='demodex-ssh-') as temporary:
    root = Path(temporary)
    ssh_port, api_port, codex_port = port(), port(), port()
    for name in ('host_key', 'client_key'):
        subprocess.run(['ssh-keygen','-q','-t','ed25519','-N','','-f',str(root/name)],check=True)
    (root/'authorized_keys').write_text((root/'client_key.pub').read_text())
    (root/'known_hosts').write_text(f'[127.0.0.1]:{ssh_port} '+(root/'host_key.pub').read_text())
    (root/'sshd_config').write_text(f'''Port {ssh_port}
ListenAddress 127.0.0.1
HostKey {root}/host_key
AuthorizedKeysFile {root}/authorized_keys
PidFile {root}/sshd.pid
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
AllowUsers {getpass.getuser()}
Subsystem sftp internal-sftp
''')
    sshd_path = shutil.which('sshd') or '/run/current-system/sw/bin/sshd'
    executors = []
    def fixture(ws):
        try:
            for raw in ws:
                r = json.loads(raw)
                if 'id' not in r: continue
                method = r['method']
                result = {}
                if method in ('thread/start','thread/resume'): result = {'thread':{'id':'fixture','turns':[]},'sandbox':{'type':'dangerFullAccess'}}
                elif method == 'thread/read': result = {'thread':{'status':{'type':'idle'}}}
                elif method == 'thread/queue/list': result = {'data':[],'nextCursor':None}
                elif method == 'thread/goal/get': result = {'goal':None}
                elif method == 'environment/add': executors.append(r['params'])
                ws.send(json.dumps({'id':r['id'],'result':result}))
        except Exception: pass
    server = serve(fixture,'127.0.0.1',codex_port)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    with (root/'sshd.log').open('w+') as sshlog, (root/'daemon.log').open('w+') as log:
        sshd = subprocess.Popen([sshd_path,'-D','-e','-f',str(root/'sshd_config')],stdout=sshlog,stderr=sshlog)
        daemon = subprocess.Popen([os.environ.get('DEMODEX_BIN',str(ROOT/'target/rust-pwa/debug/demodex')),'--bind',f'127.0.0.1:{api_port}','--data-dir',str(root/'state'),'--web-dir',str(ROOT/'web/.rust-dist'),'--host-workspace',str(root)],stdout=log,stderr=log)
        try:
            wait_port(ssh_port); wait_port(api_port)
            token = (root/'state/access-token').read_text().strip()
            def api(path, body=None, error=None):
                from wormhole_client import api as actor_api
                return actor_api(f'http://127.0.0.1:{api_port}', token, path, body, error)
            config={'name':'Disposable SSH','destination':f'{getpass.getuser()}@127.0.0.1','port':ssh_port,'identity_file':str(root/'client_key'),'known_hosts_file':str(root/'known_hosts'),'cwd':str(root)}
            api('/targets/ssh',dict(config,destination='-oProxyCommand=bad'),error='SSH config alias')
            (root/'empty_hosts').touch()
            api('/targets/ssh',dict(config,known_hosts_file=str(root/'empty_hosts')),error='host key verification failed')
            # Add through the actual Yew settings form and authenticated actor mutation.
            with sync_playwright() as playwright:
                browser=playwright.chromium.launch(executable_path='/run/current-system/sw/bin/google-chrome',headless=True,args=['--no-sandbox'])
                page=browser.new_page(viewport={'width':390,'height':844})
                page.goto(f'http://127.0.0.1:{api_port}')
                page.get_by_label('Access token').fill(token)
                page.get_by_role('button',name='Connect host',exact=True).click()
                expect(page.locator('header .indicator')).to_have_text('CONNECTED',timeout=20000)
                page.get_by_role('button',name='Server settings',exact=True).click()
                page.get_by_text('Add SSH target',exact=True).click()
                for label,value in [('SSH target name',config['name']),('SSH destination',config['destination']),('SSH port (optional)',str(ssh_port)),('Identity file on this server (optional)',config['identity_file']),('Known hosts file on this server (optional)',config['known_hosts_file']),('Remote working directory',str(root))]:
                    page.get_by_label(label,exact=True).fill(value)
                page.get_by_role('button',name='Check and add SSH target',exact=True).click()
                expect(page.locator('.target-registry h3').filter(has_text=config['name'])).to_have_text(config['name'],timeout=30000)
                page.get_by_role('button',name='Check SSH connection',exact=True).click()
                expect(page.get_by_text('SSH connection verified.',exact=True)).to_be_visible(timeout=30000)
                browser.close()
            target=next(t for t in api('/targets') if t['kind']=='ssh')
            assert next(t for t in api('/targets') if t['id']==target['id'])['kind']=='ssh'
            api('/targets/'+target['id']+'/check',{})
            direct=api('/runtime/sessions',{'name':'Direct SSH only','targets':[{'id':target['id'],'cwd':str(root)}],'sandbox':'danger-full-access'})
            assert direct['thread_id'] and len(direct['targets'])==1, direct
            assert direct['targets'][0]['id'].startswith(target['id']+'-'), direct
            direct_detail=api('/sessions/'+direct['id'])
            assert direct_detail['target_selection']==[{'id':target['id'],'cwd':str(root)}]
            assert not direct_detail['targets_pending']
            assert not any(event['message'].get('method')=='demodex/promptAccepted' for event in api('/sessions/'+direct['id']+'/events'))
            api('/sessions/'+direct['id']+'/targets',{'targets':[]})

            session=api('/sessions',{'name':'SSH fixture','endpoint':f'ws://127.0.0.1:{codex_port}','targets':[]})
            sid=session['id']
            api(f'/sessions/{sid}/connect',{})
            api(f'/sessions/{sid}/targets',{'targets':[{'id':target['id'],'cwd':str(root)}]})
            with sync_playwright() as playwright:
                browser=playwright.chromium.launch(executable_path='/run/current-system/sw/bin/google-chrome',headless=True,args=['--no-sandbox'])
                page=browser.new_page(viewport={'width':1200,'height':850})
                page.goto(f'http://127.0.0.1:{api_port}')
                page.get_by_label('Access token').fill(token)
                page.get_by_role('button',name='Connect host',exact=True).click()
                expect(page.locator('header .indicator')).to_have_text('CONNECTED',timeout=20000)
                page.get_by_role('button',name='SSH fixture',exact=False).click()
                prompt=page.get_by_label('Message',exact=True)
                png=base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=')
                with page.expect_file_chooser() as chooser:
                    page.get_by_role('button',name='Attach image',exact=True).click()
                chooser.value.set_files({'name':'capture.png','mimeType':'image/png','buffer':png})
                expect(prompt).to_have_value(re.compile(r'.*/\.demodex-upload-[^/]+/image\.png.*'),timeout=30000)
                image_path=Path(prompt.input_value().split('"')[1])
                assert image_path.read_bytes()==png
                assert image_path.parent.parent==root
                assert image_path.stat().st_mode & 0o777==0o600
                assert image_path.parent.stat().st_mode & 0o777==0o700
                browser.close()
            url=executors[-1]['execServerUrl']
            try:
                connect(url,origin='https://untrusted.example',legacy=True)
                raise AssertionError('Browser Origin accepted')
            except Exception as e:
                assert '403' in str(e),e
            c=Client(url)
            initialized=c.call('initialize',{'clientName':'integration'})
            assert initialized['environmentInfo']['platformOs']=='linux'
            uri=(root/'a file.txt').as_uri()
            encode=lambda s:base64.b64encode(s.encode()).decode()
            c.call('fs/writeFile',{'path':uri,'dataBase64':encode('hello remote')})
            assert c.call('fs/readFile',{'path':uri})['dataBase64']==encode('hello remote')
            c.call('fs/readFile',{'path':uri,'sandbox':{'permissions':{'type':'managed'}}},error='cannot enforce a sandbox')
            for method,params in [('fs/open',{'path':uri,'handleId':'file1'}),('fs/readBlock',{'handleId':'file1','offset':0,'len':6}),('fs/close',{'handleId':'file1'}),('fs/copy',{}),('fs/walk',{})]:
                c.call(method,params,error='does not support')
            c.call('fs/readFile',{'path':uri,'followSymlinks':False},error='no-follow guarantee')
            c.call('fs/getMetadata',{'path':(root/'missing').as_uri()},error='status 2')
            directory=(root/'directory').as_uri()
            c.call('fs/createDirectory',{'path':directory})
            c.call('fs/createDirectory',{'path':directory,'recursive':True},error='Recursive')
            copy=(root/'directory'/'copy.txt').as_uri()
            c.call('fs/writeFile',{'path':copy,'dataBase64':encode('copied')})
            assert c.call('fs/getMetadata',{'path':copy})['isFile']
            assert c.call('fs/canonicalize',{'path':copy})['path']==copy
            assert c.call('fs/readDirectory',{'path':directory})['entries'][0]['fileName']=='copy.txt'
            c.call('fs/remove',{'path':directory,'recursive':True},error='Recursive')
            c.call('fs/remove',{'path':copy})
            c.call('fs/remove',{'path':directory})
            assert not (root/'directory').exists()
            def params(pid,script,**extra):
                p={'processId':pid,'argv':['sh','-c',script],'cwd':root.as_uri(),'env':{},'tty':False,'pipeStdin':False}
                p.update(extra)
                return p
            def start(pid,script,**extra):
                p=params(pid,script,**extra)
                c.call('process/start',p)
                return p
            def finish(pid):
                r=c.call('process/read',{'processId':pid})
                assert r['closed'] and r['exited'] and not r['failure'],r
                return r,b''.join(base64.b64decode(ch['chunk']) for ch in r['chunks'])
            p=start('command','pwd; printf "%s\\n" "$1" "$VALUE"; sleep 0.2; printf done > completed',argv=['sh','-c','pwd; printf "%s\\n" "$1" "$VALUE"; sleep 0.2; printf done > completed','sh','literal; $(touch bad)'],env={'VALUE':'env spaces'})
            assert (root/'completed').read_text()=='done', 'process/start returned before completion'
            result,data=finish('command')
            assert result['exitCode']==0 and b'literal; $(touch bad)' in data and b'env spaces' in data,data
            assert not (root/'bad').exists()
            assert any(event['method']=='process/output' for event in c.events)
            c.call('process/start',p)
            c.call('process/start',dict(p,argv=['false']),error='different parameters')
            start('exit','exit 7')
            assert finish('exit')[0]['exitCode']==7
            start('policy','printf "%s" "$VALUE"',envPolicy={'inherit':'none','set':{'VALUE':'policy value'},'exclude':[],'includeOnly':[],'ignoreDefaultExcludes':False})
            assert finish('policy')[1]==b'policy value'
            for field in ['tty','pipeStdin']:
                c.call('process/start',params('unsupported-'+field,'true',**{field:True}),error='does not support')
            for method in ['process/write','process/signal','http/request']:
                c.call(method,{'processId':'command'},error='does not support')
            c.call('process/start',dict(p,processId='sandbox',sandbox={'permissions':{'type':'managed'}}),error='cannot enforce a sandbox')
            # Independent commands run concurrently although each start waits for completion.
            for ident,payload in [(900,params('slow','sleep 1; printf slow')),(901,params('fast','printf fast'))]:
                c.ws.send(json.dumps({'jsonrpc':'2.0','id':ident,'method':'process/start','params':payload}))
            replies=[]
            while len(replies)<2:
                value=json.loads(c.ws.recv(timeout=30))
                if 'id' in value:
                    assert 'error' not in value,value
                    replies.append(value['id'])
            assert replies==[901,900],replies
            # Cancellation closes local SSH, with no promised remote exit status.
            c.ws.send(json.dumps({'jsonrpc':'2.0','id':902,'method':'process/start','params':params('cancel','printf ready; sleep 2')}))
            while True:
                event=json.loads(c.ws.recv(timeout=30))
                if event.get('method')=='process/output' and event['params']['processId']=='cancel':break
            c.ws.send(json.dumps({'jsonrpc':'2.0','id':903,'method':'process/terminate','params':{'processId':'cancel'}}))
            replies={}
            while len(replies)<2:
                value=json.loads(c.ws.recv(timeout=30))
                if 'id' in value: replies[value['id']]=value
            assert 'remote' in replies[902]['error']['message'].lower(),replies
            outcome=c.call('process/read',{'processId':'cancel'})
            assert outcome['closed'] and not outcome['exited'] and outcome['exitCode'] is None and 'unknown' in outcome['failure']
            c.call('process/start',params('cancel','printf ready; sleep 2'),error='will not be replayed')
            start('tmux-check','command -v tmux')
            tmux_status,tmux_path=finish('tmux-check')
            if tmux_status['exitCode']==0:
                # Optional standard tmux escape hatch. The short job exits on its own.
                start('tmux-job','',argv=[tmux_path.decode().strip(),'-S',str(root/'tmux.sock'),'-f','/dev/null','new-session','-d','-s','demodex-test','printf done > '+str(root/'tmux-result')+'; sleep 1'])
                for _ in range(100):
                    if (root/'tmux-result').exists():break
                    time.sleep(.05)
                assert (root/'tmux-result').read_text()=='done'
            start('timeout-check','command -v timeout')
            timeout_status,timeout_path=finish('timeout-check')
            if timeout_status['exitCode']==0:
                start('chosen-timeout','',argv=[timeout_path.decode().strip(),'0.2','sleep','2'])
                assert finish('chosen-timeout')[0]['exitCode']==124
            if '--long-command' in sys.argv:
                start('long-command','for n in 1 2 3 4 5 6 7 8 9 10 11 12 13; do printf heartbeat; sleep 10; done; printf completed')
                assert finish('long-command')[1].endswith(b'completed'), 'Former two-minute cutoff returned'
            # Exercise the installed Codex client against our protocol, with no inference.
            app_port=port()
            profile=root/'codex-profile';profile.mkdir()
            app=subprocess.Popen(['codex','app-server','--listen',f'ws://127.0.0.1:{app_port}'],env=dict(os.environ,CODEX_HOME=str(profile)),stdout=log,stderr=log)
            try:
                wait_port(app_port)
                real=Client(f'ws://127.0.0.1:{app_port}')
                real.call('initialize',{'clientInfo':{'name':'ssh_test','version':'0'},'capabilities':{'experimentalApi':True}})
                real.ws.send(json.dumps({'method':'initialized','params':{}}))
                real.call('environment/add',{'environmentId':'ssh-test','execServerUrl':url})
                thread=real.call('thread/start',{'cwd':str(root),'sandbox':'danger-full-access','approvalPolicy':'on-request','environments':[{'environmentId':'ssh-test','cwd':str(root)}]})['thread']['id']
                assert real.call('thread/read',{'threadId':thread,'includeTurns':False})['thread']['id']==thread
                real.close()
            finally:
                app.terminate();app.wait(timeout=15)
            c.close()
            c=Client(url)
            c.call('initialize',{'clientName':'integration','resumeSessionId':initialized['sessionId']},error='unavailable')
            c.close()
            api(f'/sessions/{sid}/targets',{'targets':[{'id':target['id'],'cwd':str(root/'missing-dir')}]},error='working directory does not exist')
            old_identity=executors[-1]['environmentId']
            api('/targets/'+target['id']+'/reconnect',{})
            assert api(f'/sessions/{sid}')['session']['status']=='disconnected'
            api(f'/sessions/{sid}/connect',{})
            assert executors[-1]['environmentId']!=old_identity
            api('/targets/'+target['id']+'/forget',{},error='Detach')
            api(f'/sessions/{sid}/targets',{'targets':[]})
            api('/targets/'+target['id']+'/forget',{})
            assert not any(t['id']==target['id'] for t in api('/targets'))
            print('SSH adapter: real transport, host keys, SFTP, synchronous commands, parallel sessions, best-effort cancellation, tmux guidance, unsupported requests and target lifecycle passed')
        except BaseException:
            log.flush();sshlog.flush()
            print((root/'daemon.log').read_text()[-6000:]);print((root/'sshd.log').read_text()[-6000:])
            raise
        finally:
            daemon.terminate();sshd.terminate()
            daemon.wait(timeout=15);sshd.wait(timeout=15)
            server.shutdown()
