# /// script
# dependencies = ["websockets>=15"]
# ///
"""Real Codex restart recovery with local mock inference and disposable credentials-free profile."""
import json,os,socket,sqlite3,subprocess,tempfile,threading,time
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from pathlib import Path
from wormhole_client import call
ROOT=Path(__file__).resolve().parents[1]
BIN=os.environ.get('DEMODEX_BIN',str(ROOT/'target/rust-pwa/debug/demodex'))
hits=[];release=threading.Event()
class Mock(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_POST(self):
        import gzip
        body=self.rfile.read(int(self.headers['Content-Length']))
        if self.headers.get('Content-Encoding')=='gzip':body=gzip.decompress(body)
        hits.append(json.loads(body))
        self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
        self.wfile.write(b'data: {"type":"response.created","response":{"id":"fixture","status":"in_progress","output":[]}}\n\n');self.wfile.flush()
        release.wait(120)
def wait(fn):
    for _ in range(300):
        if fn():return
        time.sleep(.1)
    raise AssertionError('Timed out')
with tempfile.TemporaryDirectory(prefix='demodex-restart-native-') as tmp:
    root=Path(tmp);profile=root/'profile';workspace=root/'workspace';data=root/'state'
    profile.mkdir();workspace.mkdir()
    mock=ThreadingHTTPServer(('127.0.0.1',0),Mock);threading.Thread(target=mock.serve_forever,daemon=True).start()
    catalog=json.loads(subprocess.check_output(['codex','debug','models','--bundled'],env={**os.environ,'CODEX_HOME':str(profile)}))
    model=catalog['models'][0].copy();model.update(slug='gpt-fixture',tool_mode='direct',use_responses_lite=False,supports_search_tool=False)
    (profile/'models.json').write_text(json.dumps({'models':[model]}))
    (profile/'config.toml').write_text(f'''model="gpt-fixture"
model_provider="fixture"
model_catalog_json={json.dumps(str(profile/'models.json'))}
[model_providers.fixture]
name="Fixture"
base_url="http://127.0.0.1:{mock.server_port}/v1"
wire_api="responses"
requires_openai_auth=false
supports_websockets=false
''')
    with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
    log=(root/'daemon.log').open('w+');process=None
    def start():
        global process
        process=subprocess.Popen([BIN,'--api-only','--bind',f'127.0.0.1:{port}','--data-dir',str(data),'--host-workspace',str(workspace),'--codex-home',str(profile)],stdout=log,stderr=log)
        def ready():
            assert process.poll() is None,'daemon exited'
            try:
                with socket.create_connection(('127.0.0.1',port),timeout=.1):return True
            except OSError:return False
        wait(ready)
    def api(op):return call(f'http://127.0.0.1:{port}',(data/'access-token').read_text().strip(),op)
    try:
        start()
        session=api({'CreateSession':{'input':{'name':'Restart fixture','targets':[{'id':'host','cwd':str(workspace)}],'sandbox':'read-only'}}})
        api({'Prompt':{'id':session['id'],'text':'Wait for a fixture response.'}})
        wait(lambda:len(hits)>0)
        api({'Goal':{'id':session['id'],'input':{'action':'start','objective':'Continue the disposable fixture'}}})
        wait(lambda:api({'Detail':{'id':session['id']}})['controls']['goal']['status']=='active')
        process.terminate();process.wait(timeout=30)
        previous_requests=len(hits)
        with sqlite3.connect(data/'state.sqlite') as db:
            assert not db.execute("SELECT 1 FROM sqlite_master WHERE name='session_restart'").fetchone()
        start()
        wait(lambda:api({'Detail':{'id':session['id']}})['controls']['connected'])
        detail=api({'Detail':{'id':session['id']}})
        assert detail['session']['thread_id']==session['thread_id']
        assert detail['controls']['goal']['status']=='active',detail['controls']
        assert detail['session']['targets'][0]['cwd']==str(workspace)
        wait(lambda:len(hits)>previous_requests)
        print('PASS: real Codex active-goal session reconnects and continues after daemon/runtime replacement, retaining thread, goal and selected workspace; local mock inference only')
    except BaseException:
        log.flush();print((root/'daemon.log').read_text()[-6000:]);raise
    finally:
        if process and process.poll() is None:process.terminate();process.wait(timeout=30)
        release.set();mock.shutdown();log.close()
