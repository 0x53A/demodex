# /// script
# dependencies = ["websockets>=15", "playwright"]
# ///
"""Real Codex plugin defaults/overrides and model-visible filtering; local mock inference only."""
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

with tempfile.TemporaryDirectory(prefix='demodex-plugins-') as tmp:
    root=Path(tmp); profile=root/'profile';workspace=root/'workspace';data=root/'state'
    data.mkdir(mode=0o700)
    for p in (profile,workspace,data/'claude'):p.mkdir(parents=True)
    server=ThreadingHTTPServer(('127.0.0.1',0),Mock);threading.Thread(target=server.serve_forever,daemon=True).start()
    catalogue=json.loads(subprocess.check_output(['codex','debug','models','--bundled'],env={**os.environ,'CODEX_HOME':str(profile)}))
    model=catalogue['models'][0].copy();model.update(slug='gpt-fixture',tool_mode='direct',use_responses_lite=False,supports_search_tool=False,prefer_websockets=False)
    catalogue['models']=[model];(profile/'models.json').write_text(json.dumps(catalogue))
    plugin=profile/'plugins/cache/test/fixture/local';(plugin/'.codex-plugin').mkdir(parents=True)
    (plugin/'.codex-plugin/plugin.json').write_text(json.dumps({'name':'fixture','description':'Fixture plugin'}))
    (plugin/'skills/probe').mkdir(parents=True)
    (plugin/'skills/probe/SKILL.md').write_text('---\nname: probe\ndescription: fixture plugin skill\n---\nFixture skill.\n')
    mcp=root/'mcp.py';mcp.write_text('''import json,sys
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r:continue
 m=r['method']
 result={'protocolVersion':'2024-11-05','capabilities':{'tools':{}},'serverInfo':{'name':'fixture','version':'1'}} if m=='initialize' else {'tools':[{'name':'fixture_echo','description':'Plugin fixture echo','inputSchema':{'type':'object','properties':{}}}]} if m=='tools/list' else {}
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
''')
    (plugin/'.mcp.json').write_text(json.dumps({'mcpServers':{'fixture':{'command':sys.executable,'args':[str(mcp)]}}}))
    market=root/'market';(market/'.agents/plugins').mkdir(parents=True)
    (market/'.agents/plugins/marketplace.json').write_text(json.dumps({'name':'test','plugins':[{'name':'fixture','source':{'source':'local','path':'./fixture'}}]}))
    (market/'fixture').symlink_to(plugin,target_is_directory=True)
    config=f''' model="gpt-fixture"
model_provider="fixture"
model_catalog_json={json.dumps(str(profile/'models.json'))}
[model_providers.fixture]
name="Fixture"
base_url="http://127.0.0.1:{server.server_port}/v1"
wire_api="responses"
requires_openai_auth=false
supports_websockets=false
[features]
apps=false
plugins=true
[marketplaces.test]
source_type="local"
source={json.dumps(str(market))}
[plugins."fixture@test"]
enabled=true
'''
    (profile/'config.toml').write_text(config)
    claude={'id':'claude-fixture','display_name':'Claude fixture','max_input_tokens':200000,'max_tokens':32000,'capabilities':{'thinking':{'supported':True,'types':{'adaptive':{'supported':True}}}}}
    (data/'claude/account.json').write_text(json.dumps({'access':'unused-test-token','refresh':'unused-refresh','expires':int(time.time())+3600,'models':[claude]}))
    with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
    with socket.socket() as sock:sock.bind(('127.0.0.1',0));web_port=sock.getsockname()[1]
    origin=f'http://127.0.0.1:{web_port}'
    with (root/'daemon.log').open('w+') as log:
        process=subprocess.Popen([BIN,'--api-only','--data-dir',str(data),'--bind',f'127.0.0.1:{port}','--host-workspace',str(workspace),'--codex-home',str(profile),'--allowed-origin',origin],stdout=log,stderr=log)
        try:
            wait_until(lambda:(data/'access-token').exists())
            token=(data/'access-token').read_text().strip()
            def api(op):return call(f'http://127.0.0.1:{port}',token,op)
            wait_until(lambda:process.poll() is None and bool(api('Sessions') is not None))
            def create(model=None):
                s=api({'CreateSession':{'input':{'name':'Plugin fixture','targets':[{'id':'host','cwd':str(workspace)}],'sandbox':'read-only',**({'model':{'model':model,'effort':'medium','serviceTier':None}} if model else {})}}})
                assert s['status']=='connected',s
                return s
            def view(id=None):return api({'PluginSettings':{'id':id}})
            def save(old,disabled,id=None,receipt=None):return call(f'http://127.0.0.1:{port}',token,{'SavePluginSettings':{'id':id,'expected_revision':old['revision'],'expected_disabled':old['disabled'],'disabled':disabled}},receipt)
            old=create();api({'RenameSession':{'id':old['id'],'name':'Existing plugin session'}});v=view();assert any(p['id']=='fixture@test' for p in v['plugins']),v
            save(v,['fixture@test'])
            assert view(old['id'])['disabled']==[], 'Existing session followed server defaults'
            fresh=create();assert view(fresh['id'])['disabled']==['fixture@test']
            c=create('claude-fixture');assert view(c['id'])['disabled']==['fixture@test']
            save(view(),[]);assert view(fresh['id'])['disabled']==['fixture@test']
            # Stale changes cannot overwrite a newer default selection.
            try:save(v,[])
            except AssertionError as e:assert 'changed' in str(e),e
            else:raise AssertionError('Stale default write accepted')
            def prompt(s):
                before=len(hits);api({'Prompt':{'id':s['id'],'text':'Fixture turn'}})
                wait_until(lambda:len(hits)>before)
                wait_until(lambda:api({'Models':{'id':s['id']}}).get('connected',True))
                with connect(data/'runtime/ipc/app.sock') as ws:
                    wait_until(lambda:rpc(ws,'thread/read',{'threadId':s['thread_id'],'includeTurns':False})['thread']['status']['type']=='idle')
                return hits[-1]
            # Test actual model payload, not merely the saved selection flag.
            assert 'fixture_echo' in json.dumps(prompt(old)['tools']),hits[-1]
            assert 'fixture_echo' not in json.dumps(prompt(fresh)['tools']),hits[-1]
            assert 'fixture plugin skill' not in json.dumps(hits[-1]['input']), 'Disabled plugin skill leaked into first turn'
            previous=view(fresh['id']);save(previous,[],fresh['id'])
            assert 'fixture_echo' in json.dumps(prompt(fresh)['tools']),hits[-1]
            save(view(fresh['id']),['fixture@test'],fresh['id'])
            assert 'fixture_echo' not in json.dumps(prompt(fresh)['tools']),hits[-1]
            assert 'fixture_echo' in json.dumps(prompt(old)['tools']), 'Other session was modified'
            # Server restart must retain defaults and every session's own selection.
            with connect(data/'claude-runtime/ipc/app.sock') as ws:
                rpc(ws,'thread/inject_items',{'threadId':c['thread_id'],'items':[{'type':'message','role':'user','content':[{'type':'input_text','text':'Saved fixture'}]}]})
            api('RestartRuntime')
            for s in (old,fresh,c):api({'Connect':{'id':s['id']}})
            assert view(old['id'])['disabled']==[]
            assert view(fresh['id'])['disabled']==['fixture@test']
            assert view(c['id'])['disabled']==['fixture@test']
            # Importing a saved thread must retain its settings, not use current defaults.
            save(view(),['fixture@test'])
            with connect(data/'runtime/ipc/app.sock') as ws:
                imported=rpc(ws,'thread/start',{'cwd':str(workspace)})['thread']['id']
                rpc(ws,'thread/inject_items',{'threadId':imported,'items':[{'type':'message','role':'user','content':[{'type':'input_text','text':'Import fixture'}]}]})
                rpc(ws,'thread/unsubscribe',{'threadId':imported})
            imported=api({'HostSession':{'input':{'name':'Imported plugin session','thread_id':imported,'sandbox':'read-only','cwd':str(workspace)}}})
            assert imported['status']=='connected',imported
            assert view(imported['id'])['disabled']==[]
            # Mutation receipts return the original result, including after later changes.
            import uuid
            receipt=str(uuid.uuid4());initial=view()
            recorded=save(initial,[],receipt=receipt)
            save(view(),['fixture@test'])
            assert save(initial,[],receipt=receipt)==recorded
            assert view()['disabled']==['fixture@test']
            if os.environ.get('DEMODEX_WEB'):
                from playwright.sync_api import sync_playwright, expect
                web=subprocess.Popen([BIN,'web','--bind',f'127.0.0.1:{web_port}','--directory',os.environ['DEMODEX_WEB']],stdout=log,stderr=log)
                try:
                    with sync_playwright() as pw:
                        browser=pw.chromium.launch(executable_path=os.environ.get('CHROME','/run/current-system/sw/bin/google-chrome'),headless=True,args=['--no-sandbox'])
                        page=browser.new_page(viewport={'width':390,'height':844});errors=[]
                        page.on('pageerror',lambda e:errors.append(str(e)))
                        page.goto(origin)
                        page.get_by_role('button',name='+ connection',exact=True).click()
                        page.get_by_label('Host URL').fill(f'http://127.0.0.1:{port}')
                        page.get_by_label('Access token').fill(token)
                        page.get_by_role('button',name='Save and connect').click()
                        expect(page.locator('header .indicator')).to_have_text('connected to',timeout=20000)
                        page.get_by_role('button',name='Server settings',exact=True).click()
                        panel=page.locator('.plugin-settings')
                        panel.get_by_role('button',name='Load plugins',exact=True).click()
                        checkbox=panel.get_by_role('checkbox').first
                        expect(checkbox).not_to_be_checked(timeout=20000)
                        checkbox.check();panel.get_by_role('button',name='Save defaults').click()
                        expect(panel.get_by_role('button',name='Save defaults')).to_be_disabled(timeout=20000)
                        assert view()['disabled']==[]
                        assert view(fresh['id'])['disabled']==['fixture@test']
                        page.get_by_role('dialog',name='Server settings').get_by_role('button',name='Close',exact=True).click()
                        page.get_by_text('Existing plugin session',exact=True).first.click()
                        page.get_by_role('button',name='Session controls',exact=True).click()
                        panel=page.locator('.plugin-settings')
                        panel.get_by_role('button',name='Load plugins',exact=True).click()
                        checkbox=panel.get_by_role('checkbox').first
                        expect(checkbox).to_be_checked(timeout=20000)
                        checkbox.uncheck();panel.get_by_role('button',name='Apply plugins').click()
                        expect(panel.get_by_role('button',name='Apply plugins')).to_be_disabled(timeout=20000)
                        assert view(old['id'])['disabled']==['fixture@test']
                        panel.get_by_role('button',name='Use current server defaults').click()
                        expect(checkbox).to_be_checked()
                        assert view(old['id'])['disabled']==['fixture@test'], 'Defaults button auto-saved'
                        panel.get_by_role('button',name='Apply plugins').click()
                        expect(panel.get_by_role('button',name='Apply plugins')).to_be_disabled(timeout=20000)
                        assert view(old['id'])['disabled']==[]
                        assert page.get_by_role('dialog',name='Session controls').evaluate('d=>d.scrollWidth<=d.clientWidth')
                        assert not errors,errors
                        browser.close()
                        print('PASS: mobile server/session plugin UI, default copy drafts, isolation and no script errors')
                finally:web.terminate();web.wait(timeout=20)
            assert (profile/'config.toml').read_text()==config
            print('PASS: dynamic discovery, frozen defaults, stale-write rejection, actual tool filtering/re-enable, session isolation, native/Claude resume and profile preservation; mock inference only')
        except BaseException:
            log.flush();print((root/'daemon.log').read_text()[-7000:]);raise
        finally:
            process.terminate();process.wait(timeout=30);server.shutdown()
