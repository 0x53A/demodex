# /// script
# dependencies = ["websockets>=15"]
# ///
"""No-model end-to-end managed runtime / VM lifecycle test.

uv run tests/managed.py IMAGE.qcow2
Never reads or copies the user's Codex credentials and never starts inference.
"""
import base64
import json
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
import urllib.error
from websockets.sync.client import connect, unix_connect

ROOT=Path(__file__).resolve().parents[1]


def rpc(ws,method,params,ident=1):
    ws.send(json.dumps({'id':ident,'method':method,'params':params}))
    while True:
        message=json.loads(ws.recv(timeout=20))
        if message.get('id')==ident:
            assert 'error' not in message,message
            return message['result']


with tempfile.TemporaryDirectory(prefix='demodex-managed-') as temporary:
    directory=Path(temporary)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
    log=(directory/'manager.log').open('w+')
    def launch():
        process=subprocess.Popen([str(ROOT/'target/rust-pwa/debug/demodex'),'--bind',f'127.0.0.1:{port}',
            '--data-dir',str(directory),'--vm-image',str(Path(sys.argv[1]).resolve()),'--web-dir',str(ROOT/'web/dist')],stdout=log,stderr=log)
        for _ in range(100):
            try:
                with socket.create_connection(('127.0.0.1',port),timeout=.2):return process
            except OSError:time.sleep(.1)
        raise AssertionError('manager did not start')
    manager=launch()
    token=(directory/'access-token').read_text().strip()
    def api(path,body=None):
        from wormhole_client import api as actor_api
        return actor_api(f'http://127.0.0.1:{port}', token, path, body, None)
    def running(ident):
        deadline=time.monotonic()+300
        while time.monotonic()<deadline:
            environment=next(e for e in api('/environments') if e['id']==ident)
            if environment['status']=='running':return
            if environment['status']=='error':raise AssertionError(environment)
            time.sleep(1)
        raise AssertionError('VM startup timed out')
    try:
        api('/runtime/start',{})
        assert api('/runtime')['account'] is None
        assert not (directory/'runtime/home/auth.json').exists()
        print('PASS: isolated managed runtime starts without copying any login',flush=True)
        environment=api('/environments',{'name':'Lifecycle fixture','memory_mib':2048,'cpus':2,'internet':False})
        ident=environment['id']
        running(ident)
        session=api(f'/environments/{ident}/sessions',{'name':'Persistence fixture'})
        assert session['thread_id'],session
        with unix_connect(session['endpoint'].removeprefix('unix://'), compression=None) as ws:
            rpc(ws,'initialize',{'clientInfo':{'name':'managed_test','version':'0'},'capabilities':{'experimentalApi':True}})
            ws.send(json.dumps({'method':'initialized','params':{}}))
            rpc(ws,'thread/inject_items',{'threadId':session['thread_id'],'items':[{'type':'message','role':'user','content':[{'type':'input_text','text':'No-model fixture history.'}]}]},2)
        old_target=session['targets'][0]
        with connect(old_target['url']) as ws:
            rpc(ws,'initialize',{'clientName':'fixture'})
            ws.send(json.dumps({'method':'initialized','params':{}}))
            rpc(ws,'fs/writeFile',{'path':'file:///workspace/persistent','dataBase64':base64.b64encode(b'guest state retained').decode()},2)
        print('PASS: automatic overlay, SSH identity, executor deployment, session attachment and guest file write',flush=True)
        api(f'/environments/{ident}/stop',{})
        assert api('/sessions/'+session['id'])['session']['status']=='disconnected'
        assert api('/environments')[0]['status']=='stopped'
        manager.terminate();manager.wait(timeout=45)
        manager=launch()
        assert api('/environments')[0]['status']=='stopped'
        api('/runtime/start',{})
        api(f'/environments/{ident}/start',{})
        running(ident)
        api('/sessions/'+session['id']+'/connect',{})
        resumed=api('/sessions/'+session['id'])['session']
        assert resumed['thread_id']==session['thread_id']
        assert resumed['targets'][0]['id']!=old_target['id'],'executor generation must change'
        with connect(resumed['targets'][0]['url']) as ws:
            rpc(ws,'initialize',{'clientName':'fixture'})
            ws.send(json.dumps({'method':'initialized','params':{}}))
            result=rpc(ws,'fs/readFile',{'path':'file:///workspace/persistent'},2)
            assert base64.b64decode(result['dataBase64'])==b'guest state retained'
        print('PASS: manager restart, VM restart, durable thread resume, fresh target generation and disk persistence',flush=True)
        # Shutdown while the VM is active must stop owned processes cleanly.
        manager.terminate();manager.wait(timeout=45)
        assert not (directory/'runtime/ipc/app.sock').exists()
        print('PASS: graceful manager shutdown removes its runtime socket and stops the VM',flush=True)
    except BaseException:
        for path in directory.rglob('*.log'):
            print(path.relative_to(directory),path.read_text(errors='replace')[-3000:],flush=True)
        raise
    finally:
        if manager.poll() is None:
            manager.terminate()
            try:manager.wait(timeout=45)
            except subprocess.TimeoutExpired:manager.kill();manager.wait()
        log.close()
