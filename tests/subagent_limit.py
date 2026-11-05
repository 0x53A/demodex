# /// script
# dependencies = ["websockets>=15", "playwright"]
# ///
"""Per-session subagent limits with real Codex and local mock inference only."""
import json, os, socket, subprocess, sys, tempfile, threading, time
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from websockets.sync.client import unix_connect
from wormhole_client import call

ROOT=Path(__file__).resolve().parents[1]
BIN=os.environ.get('DEMODEX_BIN',str(ROOT/'target/rust-pwa/debug/demodex'))
hits=[]
class Mock(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_POST(self):
        import gzip
        data=self.rfile.read(int(self.headers['Content-Length']))
        if self.headers.get('Content-Encoding')=='gzip':data=gzip.decompress(data)
        body=json.loads(data);hits.append(body)
        item={'type':'message','id':'msg_fixture','role':'assistant','status':'completed','content':[{'type':'output_text','text':'Fixture response','annotations':[]}]}
        events=[{'type':'response.created','response':{'id':'r','status':'in_progress','output':[]}},
            {'type':'response.output_item.added','output_index':0,'item':item},
            {'type':'response.output_item.done','output_index':0,'item':item},
            {'type':'response.completed','response':{'id':'r','status':'completed','output':[item],'usage':{'input_tokens':5,'output_tokens':5,'total_tokens':10}}}]
        payload=''.join('data: '+json.dumps(e)+'\n\n' for e in events).encode()
        self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Content-Length',str(len(payload)));self.end_headers();self.wfile.write(payload)

def rpc(ws,method,params):
    ws.send(json.dumps({'id':1,'method':method,'params':params}))
    while True:
        r=json.loads(ws.recv(timeout=40))
        if r.get('id')==1:
            assert 'error' not in r,r
            return r['result']
def connect(path):
    ws=unix_connect(str(path),max_size=50*1024*1024)
    rpc(ws,'initialize',{'clientInfo':{'name':'plugins-fixture','version':'1'},'capabilities':{'experimentalApi':True}})
    return ws

def wait_until(fn):
    for _ in range(300):
        if fn():return
        time.sleep(.1)
    raise AssertionError('Timed out waiting for fixture')

with tempfile.TemporaryDirectory(prefix='demodex-subagents-') as tmp:
    root=Path(tmp); profile=root/'profile';workspace=root/'workspace';data=root/'state'
    for p in (profile,workspace):p.mkdir()
    server=ThreadingHTTPServer(('127.0.0.1',0),Mock);threading.Thread(target=server.serve_forever,daemon=True).start()
    catalogue=json.loads(subprocess.check_output(['codex','debug','models','--bundled'],env={**os.environ,'CODEX_HOME':str(profile)}))
    model=catalogue['models'][0].copy();model.update(slug='gpt-fixture',tool_mode='direct',use_responses_lite=False,supports_search_tool=False)
    (profile/'models.json').write_text(json.dumps({'models':[model]}))
    config=f'''model="gpt-fixture"
model_provider="fixture"
model_catalog_json={json.dumps(str(profile/'models.json'))}
[model_providers.fixture]
name="Fixture"
base_url="http://127.0.0.1:{server.server_port}/v1"
wire_api="responses"
requires_openai_auth=false
supports_websockets=false
[agents]
max_concurrent_threads_per_session=3
'''
    (profile/'config.toml').write_text(config)
    with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
    with (root/'daemon.log').open('w+') as log:
        process=subprocess.Popen([BIN,'--data-dir',str(data),'--bind',f'127.0.0.1:{port}','--host-workspace',str(workspace),'--codex-home',str(profile),'--web-dir',os.environ.get('DEMODEX_WEB',str(ROOT/'web/.rust-dist'))],stdout=log,stderr=log)
        try:
            wait_until(lambda:(data/'access-token').exists())
            token=(data/'access-token').read_text().strip()
            def api(op,receipt=None):return call(f'http://127.0.0.1:{port}',token,op,receipt)
            wait_until(lambda:process.poll() is None and api('Sessions') is not None)
            def create(limit):
                s=api({'CreateSession':{'input':{'name':'Subagent fixture','targets':[{'id':'host','cwd':str(workspace)}],'sandbox':'read-only','max_subagents':limit}}})
                assert s['status']=='connected',s
                return s
            def change(s,limit,receipt=None):return api({'SetSubagentLimit':{'id':s['id'],'limit':limit}},receipt)
            def reject(fn,text):
                try:fn()
                except AssertionError as e:assert text in str(e),e
                else:raise AssertionError('Invalid operation accepted')
            reject(lambda:create(0),'at least 1')
            first=create(7); other=create(2)
            reject(lambda:change(first,0),'at least 1')
            reject(lambda:change(first,5),'no saved history')
            def prompt(s):
                before=len(hits);api({'Prompt':{'id':s['id'],'text':'Fixture turn'}})
                wait_until(lambda:len(hits)>before)
                with connect(data/'runtime/ipc/app.sock') as ws:
                    wait_until(lambda:rpc(ws,'thread/read',{'threadId':s['thread_id'],'includeTurns':False})['thread']['status']['type']=='idle')
                return json.dumps(hits[-1]['input'])
            def check(s,limit):
                payload=prompt(s)
                assert f'{limit+1} available concurrency slots' in payload,payload
            check(first,7); check(other,2)
            import uuid
            receipt=str(uuid.uuid4())
            before=len(hits)
            assert change(first,5,receipt)=={'limit':5}
            assert change(first,5,receipt)=={'limit':5}
            assert len(hits)==before, 'Changing the limit sent a hidden inference turn'
            check(first,5);check(other,2)
            change(first,None);check(first,3)
            assert api({'Detail':{'id':first['id']}})['session']['thread_id']==first['thread_id']
            change(first,6)
            api('RestartRuntime')
            for s in (first,other):api({'Connect':{'id':s['id']}})
            check(first,6);check(other,2)
            assert (profile/'config.toml').read_text()==config
            if os.environ.get('DEMODEX_WEB'):
                from playwright.sync_api import sync_playwright, expect
                with sync_playwright() as pw:
                    browser=pw.chromium.launch(executable_path=os.environ.get('CHROME','/run/current-system/sw/bin/google-chrome'),headless=True,args=['--no-sandbox'])
                    page=browser.new_page(viewport={'width':390,'height':844})
                    page.goto(f'http://127.0.0.1:{port}')
                    page.get_by_role('button',name='+ connection',exact=True).click()
                    page.get_by_label('Access token').fill(token)
                    page.get_by_role('button',name='Save and connect',exact=True).click()
                    expect(page.locator('header .indicator')).to_have_text('connected to',timeout=20000)
                    page.get_by_role('button',name='+ Session',exact=True).first.click()
                    creation=page.get_by_role('dialog',name='New Session',exact=True)
                    expect(creation.get_by_label('Max concurrent subagents',exact=True)).to_have_value('')
                    creation.get_by_label('Max concurrent subagents',exact=True).fill('9')
                    creation.get_by_role('button',name='Close',exact=True).click()
                    page.locator(f'[data-session-id="{first["id"]}"]').first.click()
                    page.get_by_role('button',name='Session controls',exact=True).click()
                    dialog=page.get_by_role('dialog',name='Session controls',exact=True)
                    field=dialog.get_by_label('Max concurrent subagents',exact=True)
                    expect(field).to_have_value('6')
                    field.fill('4');dialog.get_by_role('button',name='Apply subagent limit',exact=True).click()
                    wait_until(lambda:api({'Detail':{'id':first['id']}})['controls']['subagent_limit']=={'limit':4})
                    expect(dialog.get_by_role('button',name='Apply subagent limit',exact=True)).to_be_enabled(timeout=20000)
                    field.fill('');dialog.get_by_role('button',name='Apply subagent limit',exact=True).click()
                    wait_until(lambda:api({'Detail':{'id':first['id']}})['controls']['subagent_limit']=={'limit':None})
                    assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
                    browser.close()
            print('Subagent limit creation, isolation, reconnect, default, receipts and restart passed.')
        finally:
            process.terminate()
            try:process.wait(timeout=20)
            except subprocess.TimeoutExpired:process.kill();process.wait()
            server.shutdown()
