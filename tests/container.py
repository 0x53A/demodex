# /// script
# dependencies = ["playwright", "websockets>=15"]
# ///
"""Disposable OCI executor lifecycle, browser creation and real Codex attachment without inference."""
import base64
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
from playwright.sync_api import sync_playwright, expect
from websockets.sync.client import connect

from wormhole_client import api, call

ROOT = Path(__file__).resolve().parents[1]
IMAGE = os.environ.get('DEMODEX_CONTAINER_IMAGE', 'ubuntu:24.04')
ENGINE = os.environ.get('DEMODEX_CONTAINER_ENGINE', 'docker')


def rpc(ws, method, params, ident):
    ws.send(json.dumps({'id': ident, 'method': method, 'params': params}))
    while True:
        response = json.loads(ws.recv(timeout=30))
        if response.get('id') == ident:
            assert 'error' not in response, response
            return response['result']


subprocess.run([ENGINE, 'image', 'inspect', IMAGE], check=True, stdout=subprocess.DEVNULL)
assert str(Path(subprocess.check_output(['which', 'codex'], text=True).strip()).resolve()).startswith('/nix/store/'), 'The disposable fixture expects Nix-packaged Codex'

with tempfile.TemporaryDirectory(prefix='demodex-container-') as temporary:
    root = Path(temporary)
    workspace = root / 'host-workspace'
    workspace.mkdir()
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    origin = f'http://127.0.0.1:{port}'
    with (root / 'daemon.log').open('w+') as log:
        daemon = subprocess.Popen([str(ROOT / 'target/rust-pwa/debug/demodex'), '--bind', f'127.0.0.1:{port}',
            '--data-dir', str(root / 'state'), '--web-dir', str(ROOT / 'web/.rust-dist'),
            '--host-workspace', str(workspace)], stdout=log, stderr=log)
        try:
            for _ in range(200):
                assert daemon.poll() is None, (root / 'daemon.log').read_text()
                try:
                    with socket.create_connection(('127.0.0.1', port), timeout=.1):
                        break
                except OSError:
                    time.sleep(.1)
            token = (root / 'state/access-token').read_text().strip()
            with sync_playwright() as playwright:
                browser = playwright.chromium.launch(executable_path='/run/current-system/sw/bin/google-chrome',
                    headless=True, args=['--no-sandbox'])
                page = browser.new_page(viewport={'width':390,'height':844})
                page.goto(origin)
                page.get_by_role('button', name='+ connection', exact=True).click()
                page.get_by_label('Access token').fill(token)
                page.get_by_role('button', name='Save and connect', exact=True).click()
                expect(page.locator('header .indicator')).to_have_text('connected to', timeout=20000)
                page.get_by_role('button', name='Server settings', exact=True).click()
                page.get_by_text('Create a container', exact=True).click()
                page.get_by_label('Container name', exact=True).fill('Disposable work')
                page.get_by_label('Container engine', exact=True).select_option(ENGINE)
                page.get_by_label('Local container image', exact=True).fill(IMAGE)
                page.get_by_role('button', name='Create and start container', exact=True).click()
                expect(page.locator('.container-management .environment-card')).to_contain_text('Disposable work', timeout=30000)
                browser.close()
            target = next(t for t in api(origin, token, '/targets') if t['kind'] == 'container')
            assert target['status'] == 'running' and target['available'] and target['cwd'] == '/workspace' and target['engine'] == ENGINE, target
            stable = target['id']
            ident = stable.removeprefix('container-')
            name = 'demodex-container-' + ident
            inspect = json.loads(subprocess.check_output([ENGINE, 'inspect', name], text=True))[0]
            assert inspect['State']['Running']
            network_name = 'demodex-network-' + ident
            assert set(inspect['NetworkSettings']['Networks']) == {network_name}, inspect['NetworkSettings']['Networks']
            network = json.loads(subprocess.check_output([ENGINE, 'network', 'inspect', network_name], text=True))[0]
            if ENGINE == 'docker':
                assert network['Driver'] == 'bridge'
                assert network['Options']['com.docker.network.bridge.enable_icc'] == 'false'
                assert set(network['Containers']) == {inspect['Id']}
            else:
                assert network['driver'] == 'bridge'
                assert network['options']['isolate'] == 'strict'
            if ENGINE == 'docker':
                assert inspect['NetworkSettings']['Ports']['4501/tcp'][0]['HostIp'] == '127.0.0.1'
            else:
                assert subprocess.check_output([ENGINE, 'port', name, '4501'], text=True).strip().startswith('127.0.0.1:')
            mounts = {mount['Destination']: mount for mount in inspect['Mounts']}
            assert set(mounts) == {'/workspace', '/home/agent', '/nix/store'}, mounts
            assert mounts['/nix/store']['RW'] is False
            api(origin, token, '/runtime/sessions', {'name':'Invalid container access',
                'targets':[{'id':stable,'cwd':'/workspace'}], 'sandbox':'read-only'}, error='danger-full-access')
            session = api(origin, token, '/runtime/sessions', {'name':'Container session',
                'targets':[{'id':stable,'cwd':'/workspace'}], 'sandbox':'danger-full-access'})
            assert session['thread_id'] and len(session['targets']) == 1
            first = session['targets'][0]
            assert first['id'].startswith(stable + '-')
            with connect(first['url'], legacy=True) as ws:
                rpc(ws, 'initialize', {'clientName':'demodex-container-test'}, 1)
                ws.send(json.dumps({'method':'initialized','params':{}}))
                rpc(ws, 'process/start', {'processId':'create-marker', 'argv':['sh','-c','printf persisted > marker'],
                    'cwd':'file:///workspace', 'env':{}, 'tty':False, 'pipeStdin':False}, 2)
                for attempt in range(100):
                    completed = rpc(ws, 'process/read', {'processId':'create-marker'}, 3 + attempt)
                    if completed['closed']:
                        break
                    time.sleep(.05)
                assert completed['closed'] and completed['exitCode'] == 0, completed
            workspace_file = root / 'state/containers' / ident / 'workspace/marker'
            assert workspace_file.read_text() == 'persisted'
            png = base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=')
            uploaded = call(origin, token, {'UploadImage':{'id':session['id'], 'bytes':list(png)}})['path']
            assert uploaded.startswith('/workspace/.demodex-upload-')
            assert (root / 'state/containers' / ident / 'workspace' / uploaded.removeprefix('/workspace/')).read_bytes() == png
            api(origin, token, '/containers/' + ident + '/stop', {})
            assert subprocess.run([ENGINE, 'network', 'inspect', network_name], stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL).returncode != 0
            assert not any(t['available'] for t in api(origin, token, '/targets') if t['id'] == stable)
            assert workspace_file.read_text() == 'persisted'
            api(origin, token, '/containers/' + ident + '/start', {})
            # A thread without a model turn can lack a rollout; create another
            # no-inference thread to check the replacement executor identity.
            second = api(origin, token, '/runtime/sessions', {'name':'Second container session',
                'targets':[{'id':stable,'cwd':'/workspace'}], 'sandbox':'danger-full-access'})
            assert second['targets'][0]['id'] != first['id']
            assert workspace_file.read_text() == 'persisted'
            daemon.terminate()
            daemon.wait(timeout=30)
            assert subprocess.run([ENGINE, 'container', 'inspect', name], stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL).returncode != 0
            assert subprocess.run([ENGINE, 'network', 'inspect', network_name], stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL).returncode != 0
            print(f'PASS: {ENGINE} target creation, loopback binding, filesystem mounts, real Codex attachment, commands, uploads and restart persistence')
        except BaseException:
            log.flush()
            print((root / 'daemon.log').read_text()[-5000:])
            raise
        finally:
            if daemon.poll() is None:
                daemon.terminate()
                daemon.wait(timeout=30)
