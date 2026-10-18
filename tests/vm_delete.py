# /// script
# dependencies = ["playwright"]
# ///
"""VM removal through the real Wormhole service and Yew UI; no VM boot or inference."""
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import tempfile
import time
import uuid

from playwright.sync_api import sync_playwright, expect
from wormhole_client import call

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('DEMODEX_BIN', ROOT/'target/rust-pwa/debug/demodex'))
WEB = Path(os.environ.get('DEMODEX_WEB', ROOT/'web/.rust-dist'))


def port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def wait_port(number):
    for _ in range(200):
        try:
            with socket.create_connection(('127.0.0.1', number), timeout=.1):
                return
        except OSError:
            time.sleep(.05)
    raise AssertionError('daemon did not start')


with tempfile.TemporaryDirectory(prefix='demodex-vm-delete-') as tmp:
    directory = Path(tmp)
    daemon_port, web_port = port(), port()
    host, origin = f'http://127.0.0.1:{daemon_port}', f'http://127.0.0.1:{web_port}'
    processes = []
    with (directory/'log').open('w+') as log:
        try:
            processes.append(subprocess.Popen([str(BINARY), '--bind', f'127.0.0.1:{daemon_port}', '--data-dir', str(directory/'state'), '--api-only', '--allowed-origin', origin], stdout=log, stderr=log))
            processes.append(subprocess.Popen([str(BINARY), 'web', '--bind', f'127.0.0.1:{web_port}', '--directory', str(WEB)], stdout=log, stderr=log))
            wait_port(daemon_port)
            wait_port(web_port)
            token = (directory/'state/access-token').read_text().strip()
            def command(operation, receipt=None):
                return call(host, token, operation, receipt)
            def seed_vm(name):
                ident = str(uuid.uuid4())
                with sqlite3.connect(directory/'state/state.sqlite') as db:
                    db.execute("INSERT INTO environments VALUES(?,?,512,1,0,'stopped',NULL)", (ident,name))
                return {'id':ident}
            vm = seed_vm('Removal fixture')
            ident = vm['id']
            vm_dir = directory/'state/environments'/ident
            vm_dir.mkdir(parents=True)
            (vm_dir/'disk.qcow2').write_text('disposable fixture disk')
            session = str(uuid.uuid4())
            target = {'id':'vm-fixture-generation', 'url':'ws://127.0.0.1:9', 'cwd':'/workspace'}
            selection = {'id':f'vm-{ident}', 'cwd':'/workspace'}
            # Seed a disconnected, archived attachment including a staged removal.
            with sqlite3.connect(directory/'state/state.sqlite') as db:
                db.execute("INSERT INTO sessions(id,name,endpoint,targets,status) VALUES(?,?,?,?,'disconnected')", (session,'Preserved conversation','ws://127.0.0.1:9',json.dumps([target])))
                db.execute('INSERT INTO session_order(session_id,position) VALUES(?,0)',(session,))
                db.execute('INSERT INTO session_presentation VALUES(?,?)',(session,json.dumps({'name':'Fixture','icon':'V','context_reporting':False,'context':None})))
                db.execute('INSERT INTO session_archive VALUES(?)',(session,))
                db.execute('INSERT INTO session_targets VALUES(?,?,1)', (session,json.dumps([selection])))
                db.execute("INSERT INTO staged_session_targets VALUES(?,'[]','[]')", (session,))
                db.execute('INSERT INTO session_environment VALUES(?,?)', (session,ident))
                db.execute('INSERT INTO events(session_id,message) VALUES(?,?)', (session,json.dumps({'method':'fixture/history'})))
            blocked_receipt = str(uuid.uuid4())
            try:
                command({'DeleteEnvironment': {'id':ident}}, blocked_receipt)
                raise AssertionError('attached VM was deleted')
            except AssertionError as error:
                assert 'Detach executors' in str(error), error
            with sync_playwright() as playwright:
                browser = playwright.chromium.launch(executable_path=os.environ.get('CHROME','/run/current-system/sw/bin/google-chrome'), headless=True, args=['--no-sandbox'])
                page = browser.new_page(viewport={'width':390, 'height':844})
                page.goto(origin)
                page.get_by_role('button',name='+ connection',exact=True).click()
                page.get_by_label('Host URL').fill(host)
                page.get_by_label('Access token').fill(token)
                page.get_by_role('button',name='Save and connect').click()
                expect(page.locator('header .indicator')).to_have_text('connected to',timeout=20000)
                page.get_by_role('button',name='Server settings',exact=True).click()
                card = page.locator('.vm-management .environment-card').filter(has_text='Removal fixture')
                expect(card.get_by_role('button',name='Delete VM',exact=True)).to_be_disabled()
                expect(card).to_contain_text('1 attached session(s)')
                page.once('dialog',lambda dialog:dialog.dismiss())
                card.get_by_role('button',name='Detach executors',exact=True).click()
                expect(card).to_contain_text('1 attached session(s)')
                page.once('dialog',lambda dialog:dialog.accept())
                card.get_by_role('button',name='Detach executors',exact=True).click()
                expect(card).to_contain_text('0 attached session(s)',timeout=20000)
                expect(card.get_by_role('button',name='Delete VM',exact=True)).to_be_enabled()
                expect(card.get_by_role('button',name='Detach executors',exact=True)).to_be_disabled()
                page.once('dialog',lambda dialog:dialog.dismiss())
                card.get_by_role('button',name='Delete VM',exact=True).click()
                assert vm_dir.exists()
                page.once('dialog',lambda dialog:dialog.accept())
                card.get_by_role('button',name='Delete VM',exact=True).click()
                expect(card).to_have_count(0,timeout=20000)
                assert not vm_dir.exists()
                browser.close()
            with sqlite3.connect(directory/'state/state.sqlite') as db:
                assert db.execute('SELECT targets FROM sessions WHERE id=?',(session,)).fetchone()[0] == '[]'
                assert db.execute('SELECT COUNT(*) FROM events WHERE session_id=?',(session,)).fetchone()[0] > 0
                assert db.execute('SELECT COUNT(*) FROM runtime_sessions WHERE session_id=?',(session,)).fetchone()[0] == 1
            # Recorded failures stay failures even after state changed.
            try:
                command({'DeleteEnvironment':{'id':ident}},blocked_receipt)
                raise AssertionError('failed receipt was replayed')
            except AssertionError as error:
                assert 'Detach executors' in str(error), error
            vm = seed_vm('Receipt fixture')
            receipt = str(uuid.uuid4())
            operation = {'DeleteEnvironment':{'id':vm['id']}}
            assert command(operation,receipt) == command(operation,receipt) == {'ok':True}
            assert command('Environments') == []
            print('PASS: VM attachment guards, detach, deletion confirmation, history retention and receipts')
        finally:
            for process in reversed(processes):
                process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            log.seek(0)
            if __import__('sys').exc_info()[0]:
                print(log.read()[-6000:])
