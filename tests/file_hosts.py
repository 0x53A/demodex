"""Durable file identities through real host/SFTP access, without model inference."""
import base64
import getpass
import json
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
import tempfile
import time
import uuid
from wormhole_client import call

ROOT = Path(__file__).resolve().parents[1]
BINARY = os.environ.get('DEMODEX_BIN', str(ROOT/'target/rust-pwa/debug/demodex'))

def port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]

def wait_port(number, process):
    for _ in range(300):
        assert process.poll() is None, 'fixture process exited'
        try:
            with socket.create_connection(('127.0.0.1',number),timeout=.1): return
        except OSError: time.sleep(.1)
    raise AssertionError('fixture did not start')

with tempfile.TemporaryDirectory(prefix='demodex-file-hosts-') as temp:
    root=Path(temp)
    state=root/'state'
    api_port=port()
    origin=f'http://127.0.0.1:{api_port}'
    log=(root/'daemon.log').open('w+')
    daemon=None
    sshd=None
    def launch(host=True, cwd=None):
        args=[BINARY,'--data-dir',str(state),'--bind',f'127.0.0.1:{api_port}','--api-only']
        if host: args += ['--host-workspace',str(cwd or root)]
        process=subprocess.Popen(args,stdout=log,stderr=log)
        wait_port(api_port,process)
        return process
    def stop(process):
        process.terminate()
        process.wait(timeout=20)
    try:
        daemon=launch()
        token=(state/'access-token').read_text().strip()
        def rpc(operation, request_id=None): return call(origin,token,operation,request_id)
        session=rpc({'HostSession':{'input':{'name':'File identities','cwd':str(root),'sandbox':'read-only'}}})
        sid=session['id']
        target=session['targets'][0]
        path=root/'report.txt'
        path.write_text('durable file bytes\n')
        destination='./report.txt:1'
        checks=[{'executor':target['id'],'executor_name':'This host','path':destination,'state':'not-found','checked_at_ms':1,'metadata':None,'error':None,'available':False}]
        def seed(item):
            with sqlite3.connect(state/'state.sqlite') as db:
                db.execute('INSERT INTO message_files VALUES(?,?,?,?)',(sid,item,json.dumps([target]),json.dumps([{'destination':destination,'checks':checks,'note':None}])))
        def listing(item='file'):
            return rpc({'MessageFiles':{'id':sid,'item':item}})['files'][0]['checks'][0]
        def read(item='file'):
            return rpc({'ReadMessageFile':{'id':sid,'item':item,'destination':destination,'executor':target['id']}})
        def refresh(item='file'):
            return rpc({'RefreshMessageFile':{'id':sid,'item':item,'destination':destination}})
        def bind(item, host):
            return {'BindMessageFileHost':{'id':sid,'item':item,'destination':destination,'executor':target['id'],'target':host}}
        seed('file')
        refresh()
        captured=listing()
        assert captured['host']['target']=='host',captured
        assert len(captured['host']['fingerprint'])==64
        assert Path(captured['path'])==path
        assert base64.b64decode(read()['dataBase64'])==path.read_bytes()
        seed('legacy')
        stop(daemon)
        other_directory=root/'other-workspace'
        other_directory.mkdir()
        daemon=launch(cwd=other_directory)
        detail=rpc({'Detail':{'id':sid}})
        assert detail['session']['status']=='disconnected',detail
        assert listing()['available']
        assert base64.b64decode(read()['dataBase64'])==path.read_bytes()
        refresh()
        assert listing()['host']==captured['host']
        assert not listing('legacy')['available']
        assert 'No durable host identity' in listing('legacy')['unavailable_reason']
        request_id=str(uuid.uuid4())
        paired=rpc(bind('legacy','host'),request_id)
        assert rpc(bind('legacy','host'),request_id)==paired
        assert base64.b64decode(read('legacy')['dataBase64'])==path.read_bytes()
        try: rpc(bind('legacy','host'))
        except AssertionError as error: assert 'pinned host identity' in str(error)
        else: raise AssertionError('Existing identity was rebound')
        # Identity mismatch must fail without returning the file, even with valid metadata.
        with sqlite3.connect(state/'state.sqlite') as db:
            files=json.loads(db.execute('SELECT files FROM message_files WHERE session_id=? AND item_id=?',(sid,'file')).fetchone()[0])
            files[0]['checks'][0]['host']['fingerprint']='0'*64
            db.execute('UPDATE message_files SET files=? WHERE session_id=? AND item_id=?',(json.dumps(files),sid,'file'))
        try: read()
        except AssertionError as error: assert 'identity changed' in str(error)
        else: raise AssertionError('Mismatched machine identity returned contents')
        refresh()
        assert listing()['state']=='error' and listing()['metadata'] is None
        # Host opt-in remains authoritative, even for previously pinned files.
        stop(daemon)
        daemon=launch(host=False)
        assert not listing('legacy')['available']
        try: read('legacy')
        except AssertionError as error: assert 'disabled' in str(error)
        else: raise AssertionError('Host file access worked with host mode disabled')
        # Independent SSH file access: no session or executor attachment is needed.
        ssh_port=port()
        for name in ('host_key','client_key'):
            subprocess.run(['ssh-keygen','-q','-t','ed25519','-N','','-f',str(root/name)],check=True)
        (root/'authorized_keys').write_text((root/'client_key.pub').read_text())
        (root/'known_hosts').write_text(f'[127.0.0.1]:{ssh_port} '+(root/'host_key.pub').read_text())
        (root/'sshd_config').write_text(f'''Port {ssh_port}
ListenAddress 127.0.0.1
HostKey {root}/host_key
AuthorizedKeysFile {root}/authorized_keys
PidFile {root}/sshd.pid
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
AllowUsers {getpass.getuser()}
Subsystem sftp internal-sftp
''')
        sshlog=(root/'ssh.log').open('w+')
        sshd=subprocess.Popen([shutil.which('sshd') or '/run/current-system/sw/bin/sshd','-D','-e','-f',str(root/'sshd_config')],stdout=sshlog,stderr=sshlog)
        wait_port(ssh_port,sshd)
        config={'name':'File SSH','destination':f'{getpass.getuser()}@127.0.0.1','port':ssh_port,'identity_file':str(root/'client_key'),'known_hosts_file':str(root/'known_hosts'),'cwd':str(root)}
        ssh=rpc({'RegisterSshTarget':{'input':config}})['id']
        seed('ssh')
        rpc(bind('ssh',ssh))
        assert listing('ssh')['host']['target']==ssh
        assert base64.b64decode(read('ssh')['dataBase64'])==path.read_bytes()
        stop(daemon)
        daemon=launch(host=False)
        assert base64.b64decode(read('ssh')['dataBase64'])==path.read_bytes()
        # SSH trust is checked again; an empty explicit known-hosts file must fail.
        trusted=(root/'known_hosts').read_text()
        (root/'known_hosts').write_text('')
        try: read('ssh')
        except AssertionError: pass
        else: raise AssertionError('SSH access ignored host-key verification')
        (root/'known_hosts').write_text(trusted)
        rpc({'ForgetTarget':{'id':ssh}})
        assert not listing('ssh')['available']
        try: read('ssh')
        except AssertionError as error: assert 'removed' in str(error)
        else: raise AssertionError('Forgotten SSH host remained accessible')
        print('PASS: durable native/SFTP file access while disconnected, restarts, legacy pairing, receipts, identity mismatch, host opt-in, SSH trust and removal')
    except Exception:
        print((root/'daemon.log').read_text()[-5000:])
        if (root/'ssh.log').exists(): print((root/'ssh.log').read_text()[-2000:])
        raise
    finally:
        for process in (daemon,sshd):
            if process and process.poll() is None: stop(process)
