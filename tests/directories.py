# /// script
# dependencies = []
# ///
"""Directory picker against real Codex exec-server; no model calls or credentials."""
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
from wormhole_client import call

ROOT=Path(__file__).resolve().parents[1]
BINARY=os.environ.get('DEMODEX_BIN',str(ROOT/'target/rust-pwa/debug/demodex'))
def port():
    with socket.socket() as s:
        s.bind(('127.0.0.1',0));return s.getsockname()[1]
def wait(number):
    for _ in range(150):
        try:
            with socket.create_connection(('127.0.0.1',number),timeout=.1):return
        except OSError:time.sleep(.1)
    raise AssertionError('Service unavailable')
with tempfile.TemporaryDirectory(prefix='demodex-directories-') as tmp:
    root=Path(tmp)
    workspace=root/'workspace';workspace.mkdir()
    (workspace/'🦆 space').mkdir();(workspace/'ordinary file').write_text('test')
    (workspace/'alias').symlink_to(workspace/'🦆 space')
    remote_port,daemon_port=port(),port()
    with (root/'log').open('w+') as log:
        executor=subprocess.Popen(['codex','exec-server','--listen',f'ws://127.0.0.1:{remote_port}'],env=os.environ|{'CODEX_HOME':str(root/'profile')},stdout=log,stderr=log)
        daemon=subprocess.Popen([BINARY,'--api-only','--bind',f'127.0.0.1:{daemon_port}','--data-dir',str(root/'state')],stdout=log,stderr=log)
        try:
            wait(remote_port);wait(daemon_port)
            origin=f'http://127.0.0.1:{daemon_port}'
            token=(root/'state/access-token').read_text().strip()
            target=call(origin,token,{'RegisterTarget':{'input':{'name':'Disposable executor','url':f'ws://127.0.0.1:{remote_port}','cwd':str(workspace)}}})
            def browse(target,path):return call(origin,token,{'BrowseDirectories':{'target':target,'path':str(path)}})
            data=browse(target['id'],workspace)
            assert any(e['name']=='🦆 space' for e in data['entries']),data
            assert not any(e['name']=='ordinary file' for e in data['entries']),data
            data=browse(target['id'],workspace/'alias')
            assert data['path']==str(workspace/'🦆 space') and data['entries']==[],data
            for bad,path,message in [('host',workspace,'not enabled'),('unknown',workspace,'Unknown target'),(target['id'],'relative','absolute path'),(target['id'],workspace/'missing','failed')]:
                try:browse(bad,path)
                except AssertionError as error:assert message.casefold() in str(error).casefold(),str(error)
                else:raise AssertionError('Expected directory error')
            print('PASS: real exec-server directory listing, canonical paths, Unicode, errors, host-mode isolation')
        finally:
            daemon.terminate();executor.terminate()
            daemon.wait(timeout=15);executor.wait(timeout=15)
