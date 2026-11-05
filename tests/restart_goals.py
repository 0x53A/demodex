# /// script
# dependencies = ["websockets>=15"]
# ///
"""Startup reads saved goals even for disconnected/idle sessions; recovery is connect-only."""
import json, os, socket, sqlite3, subprocess, tempfile, threading, time
from collections import Counter
from pathlib import Path
from websockets.sync.server import serve
from websockets.exceptions import ConnectionClosed
from wormhole_client import call

ROOT = Path(__file__).resolve().parents[1]
BIN = os.environ.get('DEMODEX_BIN', str(ROOT/'target/rust-pwa/debug/demodex'))
resumes = Counter()
connections = {}
failed = set()
mutations = []
states = {name: {'running': running, 'goal': goal} for name, running, goal in [
    ('resume', True, 'active'), ('failed', True, 'active'), ('manual', True, 'active'),
    ('paused', True, 'paused'), ('idle', False, 'active'), ('none', True, None),
    ('cleared', True, 'active'), ('waiting', True, 'active'),
    ('archived', False, 'paused'), ('pending', False, 'active')]}

def handler(ws):
    tid = None
    try:
        for encoded in ws:
            request=json.loads(encoded);method=request.get('method');p=request.get('params',{})
            if 'id' not in request:continue
            if method == 'initialize':result={}
            elif method == 'thread/resume':
                tid=p['threadId'];resumes[tid]+=1
                if tid in failed:
                    ws.send(json.dumps({'id':request['id'],'error':{'code':-32000,'message':'fixture reconnect failure'}}));continue
                connections[tid]=ws
                result={'thread':{'id':tid,'turns':[{'id':'turn-'+tid,'status':'inProgress' if states[tid]['running'] else 'completed','items':[]} ]}}
            elif method == 'thread/goal/get':
                goal=states[p['threadId']]['goal'];result={'goal':{'status':goal} if goal else None}
            elif method == 'thread/read':result={'thread':{'id':p['threadId'],'status':{'type':'active' if states[p['threadId']]['running'] else 'idle'}}}
            elif method in ('thread/queue/list','thread/backgroundTerminals/list'):result={'data':[],'nextCursor':None}
            elif method == 'config/read':result={'config':{}}
            else:
                mutations.append(method);result={}
            ws.send(json.dumps({'id':request['id'],'result':result}))
    except ConnectionClosed:
        pass
    finally:
        if tid and connections.get(tid) is ws:connections.pop(tid,None)

def wait(fn):
    for _ in range(200):
        if fn():return
        time.sleep(.05)
    raise AssertionError('Timed out')

with tempfile.TemporaryDirectory(prefix='demodex-restart-goals-') as temp, serve(handler,'127.0.0.1',0) as upstream:
    threading.Thread(target=upstream.serve_forever,daemon=True).start()
    root=Path(temp);data=root/'state'
    with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
    log=(root/'daemon.log').open('w+')
    process=None
    def start():
        global process
        process=subprocess.Popen([BIN,'--api-only','--bind',f'127.0.0.1:{port}','--data-dir',str(data)],stdout=log,stderr=log)
        def ready():
            assert process.poll() is None,'daemon exited'
            try:
                with socket.create_connection(('127.0.0.1',port),timeout=.1):return True
            except OSError:return False
        wait(ready)
    def stop():
        process.terminate();process.wait(timeout=30)
    def api(op):return call(f'http://127.0.0.1:{port}',(data/'access-token').read_text().strip(),op)
    def row(tid):return next(s for s in api('Sessions') if s['thread_id']==tid)
    try:
        start()
        ids={}
        for name in states:
            s=api({'ExternalSession':{'input':{'name':name,'endpoint':f'ws://127.0.0.1:{upstream.socket.getsockname()[1]}','targets':[],'thread_id':name}}})
            ids[name]=s['id'];api({'Connect':{'id':s['id']}})
        # Goal state lives in Codex, independent of daemon shutdown/status.
        connections['cleared'].send(json.dumps({'method':'thread/goal/cleared','params':{'threadId':'cleared'}}))
        states['cleared']['goal']=None
        connections['waiting'].send(json.dumps({'id':900,'method':'item/tool/requestUserInput','params':{'threadId':'waiting','questions':[]}}))
        wait(lambda:row('waiting')['status']=='waiting')
        connections['manual'].send(json.dumps({'method':'thread/goal/updated','params':{'threadId':'manual','goal':{'status':'paused'}}}))
        states['manual']['goal']='paused'
        api({'Archive':{'id':ids['archived'],'archived':True}})
        states['archived']['goal']='active'
        with sqlite3.connect(data/'state.sqlite') as db:
            db.execute('UPDATE session_targets SET pending=1 WHERE session_id=?',(ids['pending'],))
        stop();before=resumes.copy();failed.add('failed')
        start()
        expected={'resume','failed','idle','waiting'}
        wait(lambda:all(resumes[n]==before[n]+1 for n in expected))
        wait(lambda:'Automatic goal reconnect failed' in (row('failed').get('error') or ''))
        assert row('resume')['thread_id']=='resume'
        assert all(resumes[n]==before[n] for n in states if n not in expected),resumes
        assert 'pending execution-target' in row('pending')['error']
        assert not mutations,mutations
        # Failures do not retry periodically. Re-read current saved goals on
        # each new startup, including edits made while the daemon was down.
        time.sleep(2.5);assert all(resumes[n]==before[n]+1 for n in expected)
        stop();before=resumes.copy()
        for name in states:states[name]['goal']=None
        start();time.sleep(.5)
        assert resumes==before,resumes
        assert all(s['status']=='disconnected' for s in api('Sessions'))
        assert not mutations,mutations
        print('PASS: saved active goals reconnect regardless of old status; paused/cleared/archived/target-pending exclusions; visible failure, one attempt per startup, offline goal edits and no hidden turn/goal/queue mutations')
    except BaseException:
        log.flush();print((root/'daemon.log').read_text()[-8000:]);raise
    finally:
        if process and process.poll() is None:stop()
        log.close()
