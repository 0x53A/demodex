# /// script
# dependencies = []
# ///
"""Project Git through a real native Codex executor and Wormhole; no inference."""
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
from wormhole_client import api, call

ROOT = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='demodex-project-git-') as temporary:
    root = Path(temporary)
    repo = root / 'repo'
    cwd = repo / 'a' / 'b'
    cwd.mkdir(parents=True)
    subprocess.run(['git', 'init', '-b', 'main', str(repo)], check=True, capture_output=True)
    (repo / 'untracked').write_text('fixture')
    outside = root / 'outside'
    outside.mkdir()
    state = root / 'state'
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    origin = f'http://127.0.0.1:{port}'
    with (root / 'daemon.log').open('w+') as log:
        process = subprocess.Popen([os.environ.get('DEMODEX_BIN', str(ROOT/'target/rust-pwa/debug/demodex')),
            '--data-dir', str(state), '--bind', f'127.0.0.1:{port}', '--api-only', '--host-workspace', str(root)], stdout=log, stderr=log)
        try:
            for _ in range(200):
                assert process.poll() is None, 'daemon stopped'
                try:
                    with socket.create_connection(('127.0.0.1', port), timeout=.1):
                        break
                except OSError:
                    time.sleep(.1)
            token = (state/'access-token').read_text().strip()
            # A read-only session must still support read-only Git metadata.
            a = api(origin, token, '/host/sessions', {'name':'Git', 'cwd':str(cwd), 'sandbox':'read-only'})
            b = api(origin, token, '/host/sessions', {'name':'No Git', 'cwd':str(outside), 'sandbox':'read-only'})
            rows = call(origin, token, 'ProjectGit')
            found = next(row for row in rows if row['path']==str(cwd))
            assert found['state']=='repository', found
            status = found['status']
            assert status['branch']=='main' and status['oid']=='(initial)', status
            assert status['root']==str(repo) and status['parent_levels']==2, status
            assert status['untracked']==1 and status['staged']==0, status
            assert next(row for row in rows if row['path']==str(outside))['state']=='not_repository', rows
            again = call(origin, token, 'ProjectGit')
            assert sorted(rows,key=lambda r:r['path'])==sorted(again,key=lambda r:r['path']), 'cache was not reused'
            print('PASS: real native executor Git, parent repository, unborn branch, dirty state, no repository and shared cache; no inference')
        except Exception:
            log.flush()
            log.seek(0)
            print(log.read()[-12000:])
            raise
        finally:
            process.terminate()
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
