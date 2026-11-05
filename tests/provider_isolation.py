# /// script
# dependencies = ["websockets>=15"]
# ///
"""Native/Claude runtime isolation and restart guards, with no inference."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
from websockets.sync.client import unix_connect
from wormhole_client import call

ROOT = Path(__file__).resolve().parents[1]
BIN = os.environ.get('DEMODEX_BIN', str(ROOT / 'target/rust-pwa/debug/demodex'))

def rpc(ws, method, params):
    ws.send(json.dumps({'id': 1, 'method': method, 'params': params}))
    while True:
        reply = json.loads(ws.recv(timeout=30))
        if reply.get('id') == 1:
            assert 'error' not in reply, reply
            return reply['result']

def connect(path):
    ws = unix_connect(str(path))
    rpc(ws, 'initialize', {'clientInfo': {'name': 'isolation-fixture', 'version': '1'}, 'capabilities': {'experimentalApi': True}})
    ws.send(json.dumps({'method': 'initialized', 'params': {}}))
    return ws

with tempfile.TemporaryDirectory(prefix='demodex-provider-isolation-') as tmp:
    root = Path(tmp)
    profile, workspace, data = root/'profile', root/'workspace', root/'state'
    for directory in (profile, workspace, data/'claude'):
        directory.mkdir(parents=True)
    data.chmod(0o700)
    catalog = json.loads(subprocess.check_output(['codex', 'debug', 'models', '--bundled'], env={**os.environ, 'CODEX_HOME': str(profile)}))
    original = json.dumps(catalog)
    native_catalog = profile/'native-models.json'
    native_catalog.write_text(original)
    native_model = catalog['models'][0]['slug']
    config = f'model={json.dumps(native_model)}\nmodel_catalog_json={json.dumps(str(native_catalog))}\n[features]\napps=false\n'
    (profile/'config.toml').write_text(config)
    model = {'id':'claude-fixture','display_name':'Claude fixture','max_input_tokens':200000,'max_tokens':32000,'capabilities':{'thinking':{'supported':True,'types':{'adaptive':{'supported':True}}}}}
    (data/'claude/account.json').write_text(json.dumps({'access':'unused-test-token','refresh':'unused-refresh','expires':int(time.time())+3600,'models':[model]}))
    with socket.socket() as sock:
        sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
    with (root/'daemon.log').open('w+') as log:
        process = subprocess.Popen([BIN,'--api-only','--data-dir',str(data),'--bind',f'127.0.0.1:{port}','--host-workspace',str(workspace),'--codex-home',str(profile)],stdout=log,stderr=log)
        try:
            for _ in range(200):
                assert process.poll() is None, 'daemon exited'
                try:
                    with socket.create_connection(('127.0.0.1',port),timeout=.1): break
                except OSError: time.sleep(.05)
            token=(data/'access-token').read_text().strip()
            def api(operation):return call(f'http://127.0.0.1:{port}',token,operation)
            def create(model=None):
                return api({'CreateSession':{'input':{'name':'Isolation fixture','targets':[{'id':'host','cwd':str(workspace)}],'sandbox':'read-only',**({'model':{'model':model,'effort':'medium','serviceTier':None}} if model else {})}}})
            models=api('RuntimeModels')['data']
            assert any(m['model']=='claude-fixture' for m in models)
            native=create()
            assert native['status']=='connected',native
            assert not (data/'claude-runtime/ipc/app.sock').exists(), 'Native creation must not start Claude'
            claude=create('claude-fixture')
            assert claude['status']=='connected',claude
            assert native['targets']==claude['targets'], 'Executors must remain shared'
            for session, is_claude in ((native,False),(claude,True)):
                subdir='claude-runtime' if is_claude else 'runtime'
                with connect(data/subdir/'ipc/app.sock') as ws:
                    cfg=rpc(ws,'config/read',{'includeLayers':False})['config']
                    thread=rpc(ws,'thread/read',{'threadId':session['thread_id'],'includeTurns':False})['thread']
                    assert thread['modelProvider']==('demodex_mixed' if is_claude else 'openai'),thread
                    assert (cfg.get('model_provider') or 'openai')==thread['modelProvider']
                    roster=rpc(ws,'model/list',{'includeHidden':True})['data']
                    assert all(m['model'].startswith('claude-')==is_claude for m in roster)
                    if not is_claude:
                        assert cfg['model_catalog_json']==str(native_catalog)
                        assert 'demodex_mixed' not in (cfg.get('model_providers') or {})
                    rpc(ws,'thread/inject_items',{'threadId':session['thread_id'],'items':[{'type':'message','role':'user','content':[{'type':'input_text','text':'Saved isolated fixture, no inference'}]}]})
                offered=api({'Models':{'id':session['id']}})['data']
                assert all(m['model'].startswith('claude-')==is_claude for m in offered)
            # A foreign thread in the Claude app-server must block a global restart.
            with connect(data/'claude-runtime/ipc/app.sock') as ws:
                foreign=rpc(ws,'thread/start',{'model':'claude-fixture','cwd':str(workspace)})['thread']['id']
                try:api('RestartRuntime')
                except AssertionError as error:assert 'outside the managed' in str(error),error
                else:raise AssertionError('Restart ignored an unmanaged Claude thread')
                rpc(ws,'thread/delete',{'threadId':foreign})
            api('RestartClaudeRuntime')
            assert api({'Detail':{'id':native['id']}})['session']['status']=='connected'
            assert api({'Detail':{'id':native['id']}})['session']['targets']==native['targets']
            api({'Connect':{'id':claude['id']}})
            assert api({'Detail':{'id':claude['id']}})['session']['thread_id']==claude['thread_id']
            # Disconnecting Claude must not affect native model discovery or execution route.
            api('DisconnectClaudeAccount')
            assert not any(m['model'].startswith('claude-') for m in api('RuntimeModels')['data'])
            assert create()['status']=='connected'
            try:api('RestartClaudeRuntime')
            except AssertionError as error:assert 'Claude account' in str(error),error
            else:raise AssertionError('Claude started without credentials')
            assert api({'Detail':{'id':native['id']}})['session']['status']=='connected'
            try:
                api({'Model':{'id':native['id'],'input':{'model':'claude-fixture','effort':'medium','serviceTier':None}}})
            except AssertionError:pass
            else:raise AssertionError('Cross-provider model change was accepted')
            api('RestartRuntime')
            assert api('Runtime')['running']
            assert not (data/'claude-runtime/ipc/app.sock').exists()
            api({'Connect':{'id':native['id']}})
            assert api({'Detail':{'id':native['id']}})['session']['thread_id']==native['thread_id']
            assert native_catalog.read_text()==original
            assert (profile/'config.toml').read_text()==config
            print('PASS: native routing/catalogue preserved, isolated Claude runtime, shared executor, scoped models, restart guards, logout independence and native resume; no inference')
        except BaseException:
            log.flush();print((root/'daemon.log').read_text()[-6000:]);raise
        finally:
            process.terminate();process.wait(timeout=30)
