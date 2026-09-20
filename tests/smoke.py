# /// script
# dependencies = ["websockets>=15"]
# ///
"""No-model integration smoke test against the installed Codex app-server.

Run after cargo build: uv run tests/smoke.py
Uses temporary state and never reads the user's Codex authentication.
"""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.request
import urllib.error
from websockets.sync.client import connect

ROOT = Path(__file__).resolve().parents[1]


def port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


with tempfile.TemporaryDirectory(prefix='demodex-smoke-') as temporary:
    directory = Path(temporary)
    app_port, manager_port = port(), port()
    env = dict(os.environ, CODEX_HOME=str(directory / 'codex'))
    for key in ('CODEX_API_KEY', 'OPENAI_API_KEY', 'CODEX_ACCESS_TOKEN', 'CODEX_EXEC_SERVER_URL'):
        env.pop(key, None)
    env['CODEX_HOME'] = str(directory / 'codex')
    Path(env['CODEX_HOME']).mkdir()
    processes = []

    def start(*argv, **kwargs):
        process = subprocess.Popen(argv, cwd=directory, env=env, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL, **kwargs)
        processes.append(process)
        return process

    def wait_port(number):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            try:
                with socket.create_connection(('127.0.0.1', number), timeout=.2):
                    return
            except OSError:
                time.sleep(.1)
        raise AssertionError(f'port {number} did not start')

    try:
        start('codex', 'app-server', '--listen', f'ws://127.0.0.1:{app_port}')
        wait_port(app_port)
        def start_manager():
            process = start(str(ROOT / 'target/debug/demodex'), '--bind', f'127.0.0.1:{manager_port}',
                            '--data-dir', str(directory / 'manager'), '--web-dir', str(ROOT / 'web/dist'))
            wait_port(manager_port)
            return process
        manager = start_manager()
        token = (directory / 'manager/access-token').read_text().strip()

        def api(path, body=None):
            request = urllib.request.Request(f'http://127.0.0.1:{manager_port}/api'+path,
                data=None if body is None else json.dumps(body).encode(),
                headers={'Authorization': 'Bearer '+token, 'Content-Type': 'application/json'})
            try:
                with urllib.request.urlopen(request, timeout=60) as response:
                    return json.load(response)
            except urllib.error.HTTPError as error:
                raise AssertionError(error.read().decode()) from error

        targets=[]
        for index in range(2):
            executor_port=port()
            start('codex','exec-server','--listen',f'ws://127.0.0.1:{executor_port}')
            wait_port(executor_port)
            targets.append({'id':f'fixture-{index}','url':f'ws://127.0.0.1:{executor_port}','cwd':str(directory)})
        session = api('/sessions', {'name': 'smoke', 'endpoint': f'ws://127.0.0.1:{app_port}', 'targets': targets})
        path = '/sessions/'+session['id']
        api(path+'/connect', {})
        thread = api(path)['session']['thread_id']
        assert thread, 'thread was not persisted'
        # Codex does not persist a completely empty thread. Seed fixture history
        # through the protocol, without invoking inference or execution tools.
        with connect(f'ws://127.0.0.1:{app_port}') as ws:
            def rpc(method, params, ident):
                ws.send(json.dumps({'id':ident,'method':method,'params':params}))
                while True:
                    response=json.loads(ws.recv(timeout=20))
                    if response.get('id') == ident:
                        assert 'error' not in response, response
                        return response.get('result')
            rpc('initialize',{'clientInfo':{'name':'demodex_test','version':'0'},'capabilities':{'experimentalApi':True}},1)
            ws.send(json.dumps({'method':'initialized','params':{}}))
            rpc('thread/inject_items',{'threadId':thread,'items':[{'type':'message','role':'user','content':[{'type':'input_text','text':'Local no-model persistence fixture.'}]}]},2)
        # Repeated HTTP clients stand in for browser disconnect/reconnect.
        assert api(path)['session']['status'] != 'disconnected'
        manager.terminate()
        manager.wait(timeout=10)
        manager = start_manager()
        assert api(path)['session']['thread_id'] == thread
        api(path+'/connect', {})
        assert api(path)['session']['thread_id'] == thread
        assert any(e['message']['method'] == 'demodex/threadSnapshot' for e in api(path+'/events'))
        print('PASS: real app-server, two exec-server targets, thread/start, durable identity, manager restart, thread/resume; no model turn')

        base = directory / 'base.qcow2'
        subprocess.run(['qemu-img','create','-f','qcow2',str(base),'32M'], check=True, capture_output=True)
        vm = directory / 'vm'
        subprocess.run([str(ROOT/'target/debug/demodex'),'vm','create','--base',str(base),'--directory',str(vm)], check=True, capture_output=True)
        info=json.loads(subprocess.check_output(['qemu-img','info','--output=json',str(vm/'disk.qcow2')]))
        assert info['backing-filename'] == str(base)
        assert (vm/'id_ed25519').stat().st_mode & 0o777 == 0o600
        print('PASS: QCOW2 backing image and private VM SSH identity; VM not booted')
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
