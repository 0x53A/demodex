# /// script
# dependencies = ["playwright", "websockets>=15"]
# ///
"""Real Yew/Wormhole browser integration with a fake Codex; no inference."""
import importlib.util
import os
import json
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time
import urllib.request

from playwright.sync_api import sync_playwright, expect
from websockets.sync.server import serve
from websockets.sync.client import connect
from websockets.exceptions import ConnectionClosed

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("DEMODEX_BIN", ROOT / "target/rust-pwa/debug/demodex"))


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_port(number):
    for _ in range(200):
        try:
            with socket.create_connection(("127.0.0.1", number), timeout=.1):
                return
        except OSError:
            time.sleep(.05)
    raise AssertionError(f"port {number} did not start")


class Codex:
    def __init__(self):
        self.socket = None
        self.calls = []
        self.answers = []
        self.connections = 0
        self.queue = []
        self.active = False
        self.dispatched = []
        self.queue_sequence = 0
        self.settings = {'model':'fixture-current','effort':'medium','serviceTier':None}
        self.background = []
        self.goal = None
        self.snapshot_gate = None

    def handle(self, ws):
        try:
            self.connected(ws)
        except ConnectionClosed:
            pass

    def connected(self, ws):
        self.socket = ws
        self.connections += 1
        for raw in ws:
            message = json.loads(raw)
            if "method" not in message:
                self.answers.append(message)
                self.send("serverRequest/resolved", {"requestId": message["id"]})
                self.send("turn/completed", {"turn": {"id": "turn"}})
                continue
            self.calls.append(message)
            if "id" not in message:
                continue
            method = message["method"]
            result = {}
            if method in ("thread/start", "thread/resume"):
                result = {"thread": {"id": "thread", "turns": []}, "sandbox": {"type": "readOnly"},'model':self.settings['model'],'reasoningEffort':self.settings['effort'],'serviceTier':self.settings['serviceTier']}
            elif method == "config/read":
                result = {"config":{"developer_instructions":"Keep the operator's configured instructions."}}
            elif method == 'model/list':
                later = message['params'].get('cursor') == 'next'
                model, effort = ('fixture-next','future-level') if later else ('fixture-current','medium')
                result = {'data':[{'id':model,'model':model,'displayName':model,'description':'Fixture model','defaultReasoningEffort':effort,'supportedReasoningEfforts':[{'reasoningEffort':effort}],'serviceTiers':[{'id':'priority','name':'Priority'}]}],'nextCursor':None if later else 'next'}
            elif method == 'thread/read':
                result = {'thread':{'id':'thread','status':{'type':'active' if self.active else 'idle'}}}
            elif method == 'thread/settings/update':
                self.settings.update({k:v for k,v in message['params'].items() if k in self.settings})
                self.send('thread/settings/updated',{'threadSettings':{**self.settings,'sandboxPolicy':{'type':'readOnly'}}})
            elif method == 'thread/goal/get':
                if self.snapshot_gate is not None:
                    seen, release = self.snapshot_gate
                    self.snapshot_gate = None
                    seen.set()
                    assert release.wait(15), 'snapshot navigation fixture was not released'
                result = {'goal':self.goal}
            elif method == 'thread/goal/set':
                if self.goal is None: self.goal={'threadId':'thread','tokensUsed':123,'timeUsedSeconds':9,'tokenBudget':None}
                self.goal.update({k:v for k,v in message['params'].items() if k != 'threadId'})
                result={'goal':self.goal}
                self.send('thread/goal/updated',result)
            elif method == 'thread/goal/clear':
                self.goal=None
                self.send('thread/goal/cleared',{})
            elif method == "turn/start":
                self.active = True
                result = {"turn": {"id": "turn"}}
            elif method == "turn/steer":
                assert message['params']['expectedTurnId']=='turn'
                if message['params']['input'][0]['text']=='Reject steer fixture':
                    ws.send(json.dumps({'id':message['id'],'error':{'message':'active turn already ended'}}))
                    continue
                if message['params']['input'][0]['text']=='Finish-race fixture':
                    self.active=False
                    ws.send(json.dumps({'id':message['id'],'error':{'code':-32600,'message':'no active turn to steer'}}))
                    continue
                result={'turnId':'turn'}
            elif method == "thread/queue/add":
                if message['params']['input'][0]['text'] == 'Reject queue fixture':
                    ws.send(json.dumps({'id':message['id'],'error':{'message':'queue fixture rejection'}}))
                    continue
                self.queue_sequence += 1
                item = {'id':str(self.queue_sequence),'input':message['params']['input'],'clientUserMessageId':message['params']['clientUserMessageId']}
                self.queue.append(item)
                result = {'queuedSubmission':item}
            elif method == "thread/backgroundTerminals/list":
                result = {"data":list(self.background),"nextCursor":None}
            elif method == "thread/backgroundTerminals/terminate":
                before = len(self.background)
                self.background = [row for row in self.background if row["processId"] != message["params"]["processId"]]
                result = {"terminated":len(self.background) < before}
            elif method == "thread/queue/list":
                result = {'data':list(self.queue),'nextCursor':None}
            elif method == "thread/queue/delete":
                before = len(self.queue)
                self.queue = [item for item in self.queue if item['id'] != message['params']['queuedSubmissionId']]
                result = {'deleted':len(self.queue) < before}
            elif method == "thread/queue/start":
                assert not self.active
                self.dispatch_queued()
                result = {'turn':{'id':'turn'}}
            elif method == "turn/interrupt":
                self.send('turn/completed',{'turn':{'id':'turn','status':'interrupted'}})
            ws.send(json.dumps({"id": message["id"], "result": result}))
            if method == "turn/start":
                self.send("turn/started", {"turn": {"id": "turn"}})
                for i in range(40):
                    self.send("item/completed", {"item": {"id": str(i), "type": "agentMessage", "text": "Fixture transcript line " + str(i)}})

    def dispatch_queued(self):
        if not self.queue: return
        item = self.queue.pop(0)
        self.dispatched.append(item['input'][0]['text'])
        self.send('thread/queue/changed',{})
        self.send('turn/started',{'turn':{'id':'turn'}})

    def send(self, method, params, ident=None):
        if method == 'turn/started': self.active = True
        if method == 'turn/completed': self.active = False
        message = {"method": method, "params": {"threadId": "thread", **params}}
        if ident is not None:
            message["id"] = ident
        self.socket.send(json.dumps(message))
        if method == 'turn/completed' and params['turn'].get('status') != 'interrupted':
            self.dispatch_queued()


with tempfile.TemporaryDirectory(prefix="demodex-rust-web-") as temporary:
    directory = Path(temporary)
    candidate = directory / "web"
    shutil.copytree(ROOT / "web/.rust-dist", candidate)
    daemon_port, web_port, codex_port = port(), port(), port()
    origin = f"http://127.0.0.1:{web_port}"
    host = f"http://127.0.0.1:{daemon_port}"
    codex = Codex()
    server = serve(codex.handle, "127.0.0.1", codex_port)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    incompatible_port = port()
    incompatible_frames = []

    def incompatible_client(socket):
        socket.send(json.dumps({"protocol": "demodex", "version": 999, "schema": "old-schema"}))
        try:
            incompatible_frames.append(socket.recv(timeout=5))
        except (ConnectionClosed, TimeoutError):
            pass

    incompatible_server = serve(incompatible_client, "127.0.0.1", incompatible_port)
    threading.Thread(target=incompatible_server.serve_forever, daemon=True).start()
    processes = []
    log = (directory / "daemon.log").open("w+")

    def start(*args):
        process = subprocess.Popen([str(BINARY), *args], cwd=ROOT, stdout=log, stderr=log)
        processes.append(process)
        return process

    try:
        daemon = start("--bind", f"127.0.0.1:{daemon_port}", "--data-dir", str(directory / "state"), "--api-only", "--allowed-origin", origin)
        web = start("web", "--bind", f"127.0.0.1:{web_port}", "--directory", str(candidate))
        wait_port(daemon_port)
        wait_port(web_port)
        token = (directory / "state/access-token").read_text().strip()
        # Even native clients must pass compatibility before the first actor
        # handshake. Neither credentials nor any binary frames are sent here.
        for field, incompatible in [("version", 999), ("schema", "incompatible")]:
            with connect(host.replace("http:", "ws:") + "/wormhole") as peer:
                hello = json.loads(peer.recv(timeout=5))
                assert hello["protocol"] == "demodex" and len(hello["schema"]) == 64
                hello[field] = incompatible
                peer.send(json.dumps(hello))
                try:
                    peer.recv(timeout=5)
                    raise AssertionError("incompatible client entered actor transport")
                except ConnectionClosed:
                    pass

        def api(path):
            from wormhole_client import api as actor_api
            return actor_api(host, token, path, None, None)

        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(executable_path=os.environ.get("CHROME", "/run/current-system/sw/bin/google-chrome"), headless=True, args=["--no-sandbox"])
            context = browser.new_context(viewport={"width": 1200, "height": 850})
            context.add_init_script("window.fixtureSockets=[]; window.WebSocket=class extends WebSocket {constructor(...args){super(...args);window.fixtureSockets.push(this);}};")
            page = context.new_page()
            errors = []
            rest_requests = []
            page.on("pageerror", lambda error: errors.append(str(error)))
            page.on("request", lambda request: rest_requests.append(request.url) if "/api/" in request.url else None)
            page.goto(origin)
            page.set_viewport_size({"width": 390, "height": 844})
            page.get_by_role("button", name="+ connection", exact=True).click()
            page.get_by_label("Host URL").fill(f"http://127.0.0.1:{port()}")
            page.get_by_label("Access token").fill(token)
            page.get_by_role("button", name="Save and connect").click()
            expect(page.get_by_role("alert")).to_contain_text("Nearby devices", timeout=20000)
            expect(page.get_by_role("alert")).to_be_in_viewport()
            expect(page.get_by_role("button", name="Save and connect")).to_be_visible()
            page.get_by_role("button", name="Save and connect").click()
            expect(page.get_by_role("alert")).to_be_visible()
            expect(page.locator("header .indicator")).to_have_text("disconnected from", timeout=20000)
            page.get_by_label("Host URL").fill(f"http://127.0.0.1:{incompatible_port}")
            page.get_by_label("Access token").fill(token)
            page.get_by_role("button", name="Save and connect").click()
            expect(page.get_by_role("alert")).to_contain_text("Protocol mismatch", timeout=20000)
            assert not incompatible_frames, "client sent data before checking compatibility"
            page.get_by_label("Host URL").fill(host)
            page.get_by_label("Access token").fill("wrong-token")
            page.get_by_role("button", name="Save and connect").click()
            expect(page.get_by_role("alert")).to_contain_text("Access token rejected", timeout=20000)
            page.get_by_label("Access token").fill(token)
            page.get_by_role("button", name="Save and connect").click()
            expect(page.locator("header .indicator")).to_have_text("connected to", timeout=20000)
            print("CHECK: connected", flush=True)
            expect(page.get_by_role("alert")).to_have_count(0)
            page.set_viewport_size({"width": 1200, "height": 850})
            expect(page.locator('aside .host-picker')).to_have_count(0)
            expect(page.locator('header .current-server')).to_be_visible()
            expect(page.get_by_role('button',name='+ External',exact=True)).to_have_count(0)
            page.get_by_role('button',name='+ New Session',exact=True).click()
            creation=page.get_by_role('dialog',name='New Session',exact=True)
            expect(creation.get_by_role('heading',name='Executors',exact=True)).to_be_visible()
            creation.get_by_label('Session name (optional)',exact=True).fill('Preserved creation draft')
            creation.get_by_role('button',name='Close',exact=True).click()
            page.get_by_role('button',name='+ New Session',exact=True).click()
            expect(creation.get_by_label('Session name (optional)',exact=True)).to_have_value('')
            creation.press('Escape')
            expect(creation).to_have_count(0)
            from wormhole_client import api as actor_api
            actor_api(host, token, '/sessions', {'name':'Wormhole fixture','endpoint':f'ws://127.0.0.1:{codex_port}','targets':[{'id':'fixture-host','url':'ws://127.0.0.1:4501','cwd':'/home/operator/src'}]})
            page.locator('.session').filter(has_text='Wormhole fixture').click()
            # A disconnected draft becomes sendable after reconnect even while
            # the separate detail/control read is still blocked.
            composer = page.get_by_label('Message', exact=True)
            composer.fill('Draft written before reconnect')
            expect(page.get_by_role('button', name='Send', exact=True)).to_be_disabled()
            seen, release = threading.Event(), threading.Event()
            codex.snapshot_gate = (seen, release)
            try:
                page.locator(".session-error").get_by_role("button", name="Reconnect", exact=True).click()
                assert seen.wait(10), 'reconnect detail read did not start'
                expect(page.locator(".session-heading .status")).to_have_text("connected", timeout=3000)
                expect(page.get_by_role('button', name='Send', exact=True)).to_be_enabled(timeout=3000)
                expect(composer).to_have_value('Draft written before reconnect')
                assert not any(c['method']=='turn/start' for c in codex.calls)
            finally:
                release.set()
            expect(page.get_by_role('button', name='Send', exact=True)).to_be_enabled()
            composer.fill('')
            page.get_by_role("button",name="Session controls",exact=True).click()
            expect(page.get_by_role('button',name='Save executors and directories',exact=True)).to_be_enabled()
            page.get_by_label('fixture-host (primary) working directory',exact=True).fill('/home/operator/src')
            page.get_by_role('button',name='Save executors and directories',exact=True).click()
            expect(page.locator('.target-picker')).to_contain_text('Executors and directories saved. Send a message')
            assert not any(c['method']=='turn/start' for c in codex.calls)
            page.get_by_role("button",name="Close",exact=True).click()
            page.get_by_label("Message", exact=True).fill("Begin fixture")
            composer = page.get_by_label('Message', exact=True)
            expect(page.get_by_label('Enter sends',exact=True)).not_to_be_checked()
            expect(composer).to_have_attribute('enterkeyhint','enter')
            composer.press('Enter')
            expect(composer).to_have_value('Begin fixture\n')
            assert not any(c['method']=='turn/start' for c in codex.calls)
            composer.press('Shift+Enter')
            expect(page.locator(".activity")).to_contain_text("Working")
            page.get_by_role("button",name="Session controls",exact=True).click()
            expect(page.get_by_role('button',name='Save for next turn',exact=True)).to_be_enabled()
            expect(page.get_by_role('button',name='Interrupt and save',exact=True)).to_be_enabled()
            expect(page.get_by_label('Session sandbox', exact=True)).to_be_disabled()
            expect(page.locator('.model-controls .control-warning')).to_be_visible()
            expect(page.locator('.session-settings .control-warning')).to_be_visible()
            expect(page.get_by_role('button', name='Apply sandbox', exact=True)).to_have_count(0)
            page.get_by_role('button',name='Close',exact=True).click()
            expect(page.get_by_role("button", name="Reconnect", exact=True)).to_have_count(0)
            expect(page.locator("article")).to_have_count(40)
            expect(page.locator('.conversation article time.message-time')).to_have_count(40)
            expect(page.locator('.conversation article time.message-time').first).not_to_contain_text('Imported')
            assert page.locator('.conversation article time.message-time').first.get_attribute('datetime').endswith('Z')

            expect(page.locator('.weekly-usage summary')).to_have_text('Weekly: unavailable')
            expect(page.locator('.composer .context-usage')).to_have_text('Context unavailable')
            def usage_report(tokens,window=200000,thread='thread'):
                codex.send('thread/tokenUsage/updated',{'threadId':thread,'turnId':'turn','tokenUsage':{'last':{'totalTokens':tokens,'inputTokens':max(0,tokens-1679),'cachedInputTokens':max(0,tokens-4000)},'total':{'totalTokens':900000},'modelContextWindow':window}})
            usage_report(50000)
            expect(page.locator('.composer .context-usage')).to_contain_text('Context 75% remaining')
            expect(page.locator('aside .session .context-usage')).to_contain_text('Context 75% remaining')
            usage_report(10000)
            expect(page.locator('.composer .context-usage')).to_contain_text('Context 95% remaining')
            usage_report(199000,thread='another-thread')
            usage_report(10000,window=None)
            expect(page.locator('.composer .context-usage')).to_have_text('Context unavailable')
            usage_report(50000)
            expect(page.locator('.composer .context-usage')).to_contain_text('Context 75% remaining')
            expect(page.locator('.composer .context-usage meter')).to_have_attribute('value','75')
            assert page.locator('.composer .context-usage meter').evaluate("el=>getComputedStyle(el).transform")=='matrix(-1, 0, 0, 1, 0, 0)'
            # Age only the disposable fixture: no waiting or model inference.
            cold_session=api('/sessions')[0]
            with sqlite3.connect(directory/'state/state.sqlite') as db:
                db.execute("UPDATE session_usage SET value=json_set(value,'$.reported_at',?) WHERE session_id=?",(int(time.time())-3600,cold_session['id']))
                db.execute("UPDATE sessions SET status='idle' WHERE id=?",(cold_session['id'],))
            codex.send('thread/status/changed',{'status':{'type':'idle'}})
            expect(page.locator('.cache-info')).to_contain_text('Possibly cold · Last request: 48,321 input tokens.')
            expect(page.locator('aside .cache-indicator')).to_have_count(1)
            expect(page.locator('.cache-info')).to_have_attribute('role','note')
            assert page.locator('.cache-info').evaluate('el=>getComputedStyle(el).borderLeftColor')=='rgb(117, 184, 247)'
            page.set_viewport_size({'width':390,'height':844})
            expect(page.locator('.cache-info')).to_be_in_viewport()
            expect(composer).to_be_in_viewport()
            page.screenshot(path=str(ROOT/'target/review-cold-session.png'))
            page.set_viewport_size({'width':1200,'height':850})
            # A repeated usage report on resume must not pretend to warm the cache.
            usage_report(50000)
            expect(page.locator('.cache-info')).to_be_visible()
            usage_report(50001)
            expect(page.locator('.cache-info')).to_have_count(0)
            expect(page.locator('aside .cache-indicator')).to_have_count(0)
            usage_report(50000)
            with sqlite3.connect(directory/'state/state.sqlite') as db:
                db.execute('UPDATE sessions SET status=? WHERE id=?',(cold_session['status'],cold_session['id']))
            codex.send('thread/status/changed',{'status':{'type':cold_session['status']}})
            print("CHECK: usage, informational cold-cache estimate and fresh-report recovery", flush=True)
            # Large transcripts never own settings, requests, or composer scrolling.
            for i in range(400):
                codex.send('item/completed',{'item':{'id':f'long-{i}','type':'agentMessage','text':f'Long transcript message {i}\n'+'Content '*30}})
                if i % 50 == 49:
                    expect(page.locator('.conversation article')).to_have_count(41+i,timeout=20000)
            expect(page.locator('.conversation article')).to_have_count(440,timeout=20000)
            transcript=page.locator('.transcript')
            for width,height in [(1200,850),(390,844),(320,568),(700,500)]:
                page.set_viewport_size({'width':width,'height':height})
                transcript.evaluate('e=>{e.scrollTop=e.scrollHeight;e.scrollTop-=20;e.dispatchEvent(new Event("scroll"));}')
                expect(page.get_by_role('button',name='Jump to latest',exact=True)).to_be_enabled()
                before=transcript.evaluate('e=>e.scrollTop')
                codex.send('item/completed',{'item':{'id':f'append-{width}','type':'agentMessage','text':f'New incoming message {width}'}})
                expect(page.locator('.conversation')).to_contain_text(f'New incoming message {width}')
                page.wait_for_function('(before)=>Math.abs(document.querySelector(".transcript").scrollTop-before)<2',arg=before)
                composer.fill('Draft while reading')
                assert abs(transcript.evaluate('e=>e.scrollTop')-before)<2
                page.get_by_role('button',name='Session controls',exact=True).click()
                dialog=page.get_by_role('dialog',name='Session controls',exact=True)
                expect(dialog).to_be_visible()
                expect(dialog.get_by_role('button',name='Close',exact=True)).to_be_focused()
                assert dialog.evaluate('d=>d.scrollWidth<=d.clientWidth'), 'Controls overflow horizontally'
                page.screenshot(path=str(ROOT/'target'/f'review-controls-{width}.png'))
                dialog.get_by_role('heading',name='Execution',exact=True).scroll_into_view_if_needed()
                page.screenshot(path=str(ROOT/'target'/f'review-execution-{width}.png'))
                page.keyboard.press('Shift+Tab')
                assert page.evaluate('document.activeElement.closest("dialog") !== null')
                dialog.locator('.modal-body').evaluate('e=>e.scrollTop=e.scrollHeight')
                expect(dialog.get_by_role('button',name='Close',exact=True)).to_be_in_viewport()
                assert abs(transcript.evaluate('e=>e.scrollTop')-before)<2
                page.keyboard.press('Escape')
                expect(dialog).to_have_count(0)
                expect(page.get_by_role('button',name='Session controls',exact=True)).to_be_focused()
                assert abs(transcript.evaluate('e=>e.scrollTop')-before)<2
                page.get_by_role('button',name='Jump to latest',exact=True).click()
                expect(page.get_by_role('button',name='Jump to latest',exact=True)).to_have_count(0)
                assert transcript.evaluate('e=>e.scrollHeight-e.clientHeight-e.scrollTop')<=1
                expect(composer).to_be_in_viewport()
                expect(page.locator('.thread-reference')).to_be_visible()
                expect(page.locator('.composer .context-usage')).to_be_visible()
                expect(page.get_by_label('Enter sends',exact=True)).to_be_in_viewport()
                if width == 320:
                    assert page.locator('.composer-actions').evaluate('e=>new Set(Array.from(e.children).map(c=>Math.round(c.getBoundingClientRect().top))).size>1')
                    assert transcript.evaluate('e=>e.clientHeight') >= 64, 'Fixed controls crowd out the transcript'
                page.screenshot(path=str(ROOT/'target'/f'review-conversation-{width}.png'))
                assert page.evaluate('document.body.scrollHeight <= innerHeight+1 && document.body.scrollWidth <= innerWidth')
            page.set_viewport_size({'width':1200,'height':850})
            page.get_by_label('Enter sends',exact=True).check()
            expect(page.locator('#composer-shortcut')).to_contain_text('Enter: send · Shift+Enter: newline')
            expect(composer).to_have_attribute('enterkeyhint','send')
            # Persistent preference is shared, while each tab keeps its own draft.
            assert page.evaluate("localStorage.getItem('demodex-enter-sends')")=='true'
            preference_tab=context.new_page()
            preference_tab.goto(origin)
            preference_tab.locator('.connection').filter(has=preference_tab.locator('small',has_text=host)).locator('.connection-open').click()
            expect(preference_tab.locator('header .indicator')).to_have_text('connected to')
            preference_tab.locator('.session').filter(has_text='Wormhole fixture').click()
            peer_toggle=preference_tab.get_by_label('Enter sends',exact=True)
            peer_composer=preference_tab.get_by_label('Message',exact=True)
            expect(peer_toggle).to_be_checked()
            composer.fill('Original tab draft')
            peer_composer.fill('Other tab draft')
            sends_before=sum(c['method'] in ('turn/start','turn/steer') for c in codex.calls)
            peer_toggle.uncheck()
            expect(page.get_by_label('Enter sends',exact=True)).not_to_be_checked()
            expect(composer).to_have_attribute('enterkeyhint','enter')
            expect(composer).to_have_value('Original tab draft')
            page.get_by_label('Enter sends',exact=True).check()
            expect(peer_toggle).to_be_checked()
            expect(peer_composer).to_have_attribute('enterkeyhint','send')
            expect(peer_composer).to_have_value('Other tab draft')
            preference_tab.reload()
            expect(peer_toggle).to_be_checked()
            expect(peer_composer).to_have_value('Other tab draft')
            assert sum(c['method'] in ('turn/start','turn/steer') for c in codex.calls)==sends_before
            preference_tab.close()
            print('CHECK: Enter sends persists and syncs both ways without syncing drafts or sending',flush=True)
            composer.fill('Steer current work')
            composer.press('Shift+Enter')
            expect(composer).to_have_value('Steer current work\n')
            composer.dispatch_event('keydown',{'key':'Enter','isComposing':True})
            composer.dispatch_event('keydown',{'key':'Enter','repeat':True})
            expect(composer).to_have_value('Steer current work\n')
            composer.fill('Steer current work')
            composer.press('Enter')
            expect(composer).to_have_value('')
            assert sum(c['method']=='turn/steer' for c in codex.calls)==1
            cold_start=time.monotonic()
            page.reload()
            expect(page.locator('[data-item-id="long-399"]')).to_have_count(1,timeout=10000)
            print(f"CHECK: cold 440-message transcript restored in {time.monotonic()-cold_start:.2f}s",flush=True)
            expect(page.get_by_label('Enter sends',exact=True)).to_be_checked()
            expect(page.get_by_label('Message', exact=True)).to_have_attribute('enterkeyhint','send')
            page.get_by_label('Enter sends',exact=True).uncheck()
            expect(page.get_by_label('Message', exact=True)).to_have_attribute('enterkeyhint','enter')
            pending_steer = page.locator('article.user').filter(has_text='Steer current work')
            expect(pending_steer).to_contain_text('Waiting for the next tool call')
            codex.send('item/started', {'turnId':'turn','item':{'id':'steer-consumed','type':'userMessage','content':[{'type':'text','text':'Steer current work'}]}})
            expect(page.locator('[data-item-id="steer-consumed"]')).to_be_visible()
            expect(pending_steer).to_have_count(1)
            expect(pending_steer).not_to_contain_text('Waiting for the next tool call')
            assert not codex.queue
            composer.fill('Reject steer fixture')
            page.get_by_role('button',name='Send',exact=True).click()
            expect(page.get_by_role('alert')).to_contain_text('active turn already ended')
            expect(composer).to_have_value('Reject steer fixture')
            assert sum(c['method']=='turn/start' for c in codex.calls)==1
            assert not codex.queue
            page.get_by_role('button',name='Dismiss',exact=True).click()
            composer.fill('Queued first')
            expect(page.get_by_role('button',name='Send',exact=True)).to_be_enabled()
            # IME composition and key repeat must never send a message.
            composer.dispatch_event('keydown',{'key':'Enter','shiftKey':True,'isComposing':True})
            composer.dispatch_event('keydown',{'key':'Enter','shiftKey':True,'repeat':True})
            expect(composer).to_have_value('Queued first')
            assert not codex.queue
            page.get_by_role('button',name='Queue for later',exact=True).click()
            expect(composer).to_have_value('')
            expect(page.locator('.queued-message')).to_have_count(1)
            assert page.locator('.queued-message').evaluate('e=>{const text=e.querySelector("pre").getBoundingClientRect(),button=e.querySelector("button").getBoundingClientRect();return button.left>=text.right && Math.abs(button.top-text.top)<1}')
            expect(page.get_by_text('Sent to Codex; waiting for the current work to finish. Interrupt pauses the queue.',exact=True)).to_have_count(0)
            composer.fill('Queued second')
            page.get_by_role('button',name='Queue for later',exact=True).click()
            expect(page.locator('.queued-message')).to_have_count(2)
            composer.fill('Cancel me')
            page.get_by_role('button',name='Queue for later',exact=True).click()
            expect(page.locator('.queued-message')).to_have_count(3)
            page.locator('.queued-message').filter(has_text='Cancel me').get_by_role('button').click()
            expect(page.locator('.queued-message')).to_have_count(2)
            page.reload()
            expect(page.locator('.queued-message')).to_have_count(2,timeout=20000)
            expect(page.locator('.composer .context-usage')).to_contain_text('Context 75% remaining')
            assert sum(c['method']=='thread/queue/add' for c in codex.calls)==3
            assert sum(c['method']=='turn/start' for c in codex.calls)==1
            assert sum(c['method']=='turn/steer' for c in codex.calls)==2
            # Codex advances its queue; the browser never resubmits queued prompts.
            codex.send('turn/completed',{'turn':{'id':'turn','status':'completed'}})
            expect(page.locator('.queued-message')).to_have_count(1)
            codex.send('turn/completed',{'turn':{'id':'turn','status':'completed'}})
            expect(page.locator('.queued-message')).to_have_count(0)
            assert codex.dispatched==['Queued first','Queued second']
            composer.fill('After interruption')
            page.get_by_role('button',name='Queue for later',exact=True).click()
            expect(page.locator('.queued-message')).to_have_count(1)
            page.get_by_role('button',name='Interrupt',exact=True).click()
            expect(page.get_by_role('button',name='Resume queue',exact=True)).to_be_visible()
            assert codex.dispatched==['Queued first','Queued second']
            page.get_by_role('button',name='Resume queue',exact=True).click()
            expect(page.locator('.queued-message')).to_have_count(0)
            expect(page.locator('.activity')).to_contain_text('Working')
            composer.fill('Reject queue fixture')
            page.get_by_role('button',name='Queue for later',exact=True).click()
            expect(page.get_by_role('alert')).to_contain_text('queue fixture rejection')
            expect(composer).to_have_value('Reject queue fixture')
            page.get_by_role('button',name='Dismiss',exact=True).click()
            session_id = api("/sessions")[0]["id"]
            page.get_by_label("Message", exact=True).fill("unsent draft")
            codex.send("item/tool/requestUserInput", {"questions": [{"id": "choice", "question": "Which workspace?", "options": [{"label": "One", "description": "First workspace"}]}]}, 777)
            question = page.get_by_label("Which workspace?", exact=False)
            question.fill("Explicit answer")
            composer.fill('Steer while waiting')
            page.get_by_role('button',name='Send',exact=True).click()
            expect(composer).to_have_value('')
            assert not codex.answers, 'Steering answered a pending question'
            expect(question).to_have_value('Explicit answer')
            composer.fill('Follow up while waiting')
            expect(page.get_by_role('button',name='Send',exact=True)).to_be_enabled()
            page.get_by_role('button',name='Queue for later',exact=True).click()
            expect(page.locator('.queued-message')).to_have_count(1)
            expect(question).to_have_value('Explicit answer')
            assert not codex.answers
            page.locator('.queued-message').get_by_role('button',name='Cancel queued message').click()
            expect(page.locator('.queued-message')).to_have_count(0)
            composer.fill('unsent draft')
            print("CHECK: transcript scrolling and modal focus", flush=True)
            # Both viewport sizes retain the header/composer while the transcript scrolls.
            for width, height in [(1200, 850), (390, 844)]:
                page.set_viewport_size({"width": width, "height": height})
                frame = page.evaluate("""() => { const r=s=>document.querySelector(s).getBoundingClientRect(); return {header:r('header').top, composer:r('.composer').bottom, height:innerHeight, body:document.body.scrollHeight, transcript:document.querySelector('.transcript').scrollHeight}; }""")
                assert frame["header"] == 0 and frame["composer"] <= height + 1, frame
                assert frame["body"] <= height + 1 and frame["transcript"] > height, frame
            page.get_by_role("button", name="Connections", exact=True).click()
            expect(page.get_by_role("heading", name="Connections", exact=True)).to_be_visible()
            page.go_back()
            expect(page.get_by_label("Message", exact=True)).to_have_value("unsent draft")
            page.go_forward()
            expect(page.get_by_role("heading", name="Connections", exact=True)).to_be_visible()
            page.go_back()
            expect(page.get_by_label("Message", exact=True)).to_have_value("unsent draft")
            # Hold an incremental snapshot while history navigation clears and
            # reopens the same conversation. Its tail must not hide the prefix.
            expect(page.locator('.transcript')).to_contain_text('Fixture transcript line 0')
            seen, release = threading.Event(), threading.Event()
            codex.snapshot_gate = (seen, release)
            page.evaluate("window.dispatchEvent(new Event('online'))")
            assert seen.wait(10), 'incremental snapshot did not start'
            try:
                codex.send('item/completed', {'item': {'id': 'navigation-tail', 'type': 'agentMessage', 'text': 'Message arriving during navigation'}})
                # A blocked goal/control read must not delay transcript delivery.
                expect(page.locator('.transcript')).to_contain_text('Message arriving during navigation', timeout=3000)
                codex.send('item/agentMessage/delta', {'itemId':'navigation-tail','delta':' — streamed while controls are blocked'})
                expect(page.locator('.transcript')).to_contain_text('streamed while controls are blocked', timeout=3000)
                assert not release.is_set()
                session_id = api('/sessions')[0]['id']
                for _ in range(100):
                    if any(event['message'].get('params', {}).get('item', {}).get('id') == 'navigation-tail'
                           for event in api(f'/sessions/{session_id}/events')):
                        break
                    time.sleep(.02)
                else:
                    raise AssertionError('navigation tail event was not persisted')
                page.get_by_role("button", name="Connections", exact=True).click()
                page.go_back()
                expect(page.locator('.transcript')).to_contain_text('Fixture transcript line 0', timeout=3000)
                expect(page.get_by_label('Message', exact=True)).to_have_value('unsent draft')
            finally:
                release.set()
            expect(page.locator('.transcript')).to_contain_text('Fixture transcript line 0', timeout=20000)
            expect(page.locator('.transcript')).to_contain_text('Message arriving during navigation')
            history_length = page.evaluate("history.length")
            assert token not in page.evaluate("JSON.stringify(history.state)")
            page.reload()
            expect(page.get_by_label("Message", exact=True)).to_have_value("unsent draft")
            assert page.evaluate("history.length") == history_length
            expect(page.get_by_label("Message", exact=True)).to_have_value("unsent draft", timeout=20000)
            expect(question).to_have_value("Explicit answer")
            assert codex.connections == 1 and not codex.answers
            assert sum(c.get("method") == "turn/start" for c in codex.calls) == 1
            # Close only the browser transport. The daemon's Codex socket and
            # pending question must survive automatic reconnect.
            page.get_by_role("button", name="Connections", exact=True).click()
            page.get_by_role("button", name="+ connection", exact=True).click()
            page.get_by_label("Host URL").fill("http://127.0.0.1:1")
            page.get_by_label("Access token").fill("unsaved-token")
            page.get_by_role("dialog", name="Add connection", exact=True).get_by_role("button", name="Close", exact=True).click()
            page.get_by_role("dialog", name="Connections", exact=True).get_by_role("button", name="Close", exact=True).click()
            page.evaluate("window.fixtureSockets.forEach(socket=>socket.close())")
            expect(page.locator("header .indicator")).to_have_text("disconnected from", timeout=20000)
            page.evaluate("window.dispatchEvent(new Event('online'))")
            expect(page.locator("header .indicator")).to_have_text("connected to", timeout=20000)
            expect(question).to_have_value("Explicit answer")
            assert codex.connections == 1 and not codex.answers
            # Publish another static release while the daemon keeps the same live RPC.
            page.wait_for_function("!!navigator.serviceWorker.controller")
            with (candidate / "pwa.js").open("a") as output:
                output.write("\n// second fixture release\n")
            spec = importlib.util.spec_from_file_location("build_web", ROOT / "tools/build-web.py")
            builder = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(builder)
            builder.seal_release(candidate)
            page.evaluate("async () => (await navigator.serviceWorker.getRegistration()).update()")
            expect(page.get_by_role("button", name="Update now")).to_be_visible(timeout=20000)
            assert not codex.answers and daemon.poll() is None
            page.get_by_role("button", name="Update now").click()
            expect(page.get_by_role("button", name="Update now")).to_have_count(0, timeout=20000)
            expect(question).to_have_value("Explicit answer", timeout=20000)
            expect(page.get_by_label("Message", exact=True)).to_have_value("unsent draft")
            assert codex.connections == 1 and not codex.answers
            page.get_by_role("button", name="Send answer", exact=True).click()
            expect(page.locator(".session-heading .status")).to_have_text("idle")
            assert codex.answers == [{"id": 777, "result": {"answers": {"choice": {"answers": ["Explicit answer"]}}}}], codex.answers
            assert api(f"/sessions/{session_id}")["session"]["status"] == "idle"
            # Approval summaries stay inert and require an explicit action on
            # both mobile and desktop; expanding details must never answer.
            for index, (width, label, decision) in enumerate([
                (390, 'Approve once', 'accept'),
                (1200, 'Decline', 'decline'),
                (390, 'Cancel turn', 'cancel'),
            ]):
                page.set_viewport_size({'width': width, 'height': 844})
                request_id = 790 + index
                before = len(codex.answers)
                command = 'echo "<img src=x onerror=alert(1)>"'
                codex.send('item/commandExecution/requestApproval', {
                    'threadId': 'thread', 'turnId': 'turn', 'itemId': f'approval-{index}',
                    'command': command, 'cwd': '/workspace/project',
                    'environmentId': 'fixture-executor', 'reason': 'Needs access to the workspace',
                }, request_id)
                card = page.get_by_role('region', name='Approve command', exact=True)
                expect(card).to_contain_text('Waiting for you')
                expect(card.locator('.approval-command')).to_have_text(command)
                expect(card.locator('.approval-context')).to_contain_text('/workspace/project')
                expect(card.locator('img')).to_have_count(0)
                expect(card.locator('.approval-details pre')).not_to_be_visible()
                card.get_by_text('Request details', exact=True).click()
                expect(card.locator('.approval-details pre')).to_contain_text('fixture-executor')
                assert len(codex.answers) == before
                assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
                card.get_by_role('button', name=label, exact=True).click()
                expect(card).to_have_count(0)
                assert codex.answers[-1] == {'id': request_id, 'result': {'decision': decision}}
            codex.send('item/fileChange/requestApproval', {
                'threadId': 'thread', 'turnId': 'turn', 'itemId': 'file-approval',
                'reason': 'Update the shared configuration', 'grantRoot': '/workspace/config',
            }, 793)
            file_card = page.get_by_role('region', name='Approve file changes', exact=True)
            expect(file_card).to_contain_text('Update the shared configuration')
            expect(file_card.locator('.approval-context')).to_contain_text('/workspace/config')
            file_card.get_by_role('button', name='Decline', exact=True).click()
            expect(file_card).to_have_count(0)
            second_port = port()
            second_host = f"http://127.0.0.1:{second_port}"
            start("--bind", f"127.0.0.1:{second_port}", "--data-dir", str(directory / "second"), "--api-only", "--allowed-origin", origin)
            wait_port(second_port)
            second_token = (directory / "second/access-token").read_text().strip()
            page.set_viewport_size({"width": 1200, "height": 850})
            page.get_by_role("button", name="+ New Session", exact=True).click()
            page.get_by_label("Session name (optional)", exact=True).fill("First server draft")
            page.get_by_role("dialog").press("Escape")
            page.get_by_role("button",name="Connections",exact=True).click()
            page.get_by_role("button", name="+ connection", exact=True).click()
            page.get_by_label("Host URL").fill(second_host)
            expect(page.get_by_label("Access token")).to_have_value("")
            page.get_by_label("Access token").fill(second_token)
            page.get_by_role("button", name="Save and connect").click()
            expect(page.locator("header .indicator")).to_have_text("connected to")
            expect(page.locator("aside .session")).to_have_count(0)
            page.get_by_role("button", name="+ New Session", exact=True).click()
            expect(page.get_by_label("Session name (optional)", exact=True)).to_have_value("")
            page.get_by_label("Session name (optional)", exact=True).fill("Second server draft")
            page.get_by_role("dialog").press("Escape")
            page.get_by_role("button",name="Connections",exact=True).click()
            page.locator(".connection-open").filter(has=page.locator("small", has_text=host)).click()
            page.locator(".session").filter(has_text="Wormhole fixture").click()
            expect(page.get_by_label("Message", exact=True)).to_have_value("unsent draft")
            page.get_by_role("button", name="+ New Session", exact=True).click()
            expect(page.get_by_label("Session name (optional)", exact=True)).to_have_value("")
            page.get_by_role("dialog").press("Escape")
            # A fresh PWA window has no sessionStorage, but retains named hosts
            # and their distinct tokens in localStorage.
            fresh = context.new_page()
            fresh.set_viewport_size({"width": 390, "height": 844})
            fresh.goto(origin)
            expect(fresh.get_by_role("heading", name="Connections", exact=True)).to_be_visible()
            expect(fresh.locator(".connection")).to_have_count(2)
            first = fresh.locator(".connection").filter(has=fresh.locator("small", has_text=host))
            first.get_by_role("button", name="Edit", exact=True).click()
            expect(fresh.get_by_role("dialog", name="Edit connection", exact=True)).to_be_visible()
            expect(fresh.get_by_label("Access token")).to_have_value(token)
            fresh.get_by_role("dialog", name="Edit connection", exact=True).get_by_role("button", name="Close", exact=True).click()
            expect(fresh.get_by_role("dialog", name="Connections", exact=True)).to_be_visible()
            first.get_by_role("button", name="Edit", exact=True).click()
            fresh.get_by_label("Connection name (optional)").fill("Laptop fixture")
            fresh.get_by_role("button", name="Save and connect", exact=True).click()
            expect(fresh.locator("header .indicator")).to_have_text("connected to")
            fresh.get_by_role("button", name="Connections", exact=True).click()
            expect(fresh.get_by_role("button", name="Laptop fixture", exact=False)).to_be_visible()
            second = fresh.locator(".connection").filter(has=fresh.locator("small", has_text=second_host))
            second.locator(".connection-open").click()
            expect(fresh.locator("header .indicator")).to_have_text("connected to")
            expect(fresh.locator("aside .session")).to_have_count(0)
            fresh.go_back()
            expect(fresh.get_by_role("heading", name="Connections", exact=True)).to_be_visible()
            fresh.locator(".connection").filter(has=fresh.locator("small", has_text=second_host)).get_by_role("button", name="Delete").click()
            expect(fresh.locator(".connection")).to_have_count(1)
            assert second_token not in fresh.evaluate("localStorage.getItem('demodex-connections')")
            fresh.close()
            assert codex.connections == 1
            # Session identity is separate from title, thread UUID and executor cwd.
            page.get_by_role("button",name="Session controls",exact=True).click()
            page.set_viewport_size({"width":1200,"height":850})
            page.locator('.transcript').evaluate('e=>e.scrollTop=0')
            expect(page.get_by_label("Codex thread UUID", exact=True)).to_have_value("thread")
            expect(page.get_by_label("Demodex session UUID", exact=True)).to_have_value(session_id)
            page.get_by_label("Demodex session UUID", exact=True).click()
            assert page.get_by_label("Demodex session UUID", exact=True).evaluate('e=>e.selectionEnd-e.selectionStart') == len(session_id)
            start_call = next(c for c in codex.calls if c.get("method") == "thread/start")
            assert start_call['params']['dynamicTools'][0]['name'] == 'demodex'
            assert 'set_user_visible_session_context' in start_call['params']['developerInstructions']
            assert start_call['params']['developerInstructions'].startswith("Keep the operator's configured instructions.")
            call = {"namespace":"demodex","tool":"set_user_visible_session_context","turnId":"turn","callId":"context-one","arguments":{"environment_id":api("/sessions")[0]["targets"][0]["id"],"path":"/home/operator/src/cairn","description":"Building the snapshot store"}}
            codex.send("item/tool/call",call,880)
            expect(page.locator('.context-project')).to_contain_text("/home/operator/src/cairn")
            expect(page.locator('.context-description')).to_have_text("Building the snapshot store")
            expect(page.locator('.tree-folder > .folder-name')).to_contain_text("/home/operator/src/cairn")
            current = api('/sessions')[0]
            identity = current['presentation']['name']
            assert current['targets'][0]['cwd'] == '/home/operator/src'
            assert current['name'] == 'Wormhole fixture'
            tool_names=[tool['name'] for tool in start_call['params']['dynamicTools'][0]['tools']]
            assert 'set_session_identity' in tool_names
            identity_call = {'namespace':'demodex','tool':'set_session_identity','turnId':'turn','callId':'identity-update','arguments':{'title':'A new title','name':'Suspicious Raven','icon':'🐦‍⬛'}}
            codex.send('item/tool/call',identity_call,883)
            expect(page.locator('.session-heading h1')).to_have_text('A new title')
            expect(page.locator('.session-heading .agent-name')).to_contain_text('Suspicious Raven')
            expect(page.locator('.tree-agent .session.chosen .agent-icon')).to_have_text('🐦‍⬛')
            codex.send('item/tool/call',{**identity_call,'callId':'identity-restore','arguments':{'title':current['name'],'name':identity,'icon':current['presentation']['icon']}},884)
            expect(page.locator('.session-heading h1')).to_have_text('Wormhole fixture')
            expect(page.locator('.session-heading .agent-name')).to_contain_text(identity)
            expect(page.locator('.session-heading h1')).to_have_text('Wormhole fixture')
            assert any(a['id']==880 and a['result']['success'] for a in codex.answers)
            codex.send("item/tool/call",{**call,"callId":"context-two","arguments":{**call['arguments'],"path":"/home/operator/src/cairn-next"}},881)
            expect(page.locator('.context-project')).to_contain_text('/home/operator/src/cairn-next')
            codex.send("item/tool/call",call,882)
            for _ in range(100):
                if any(a['id']==882 for a in codex.answers): break
                time.sleep(.02)
            assert any(a['id']==882 and a['result']['success'] for a in codex.answers)
            assert api('/sessions')[0]['presentation']['context']['path']=='/home/operator/src/cairn-next'
            page.reload()
            expect(page.locator('.session-heading .agent-name')).to_contain_text(identity)
            expect(page.locator('.session-heading h1')).to_have_text('Wormhole fixture')
            expect(page.get_by_label('Message',exact=True)).to_have_value('unsent draft')
            page.locator('.transcript').evaluate('e=>e.scrollTop=0')
            page.screenshot(path='/tmp/demodex-overview-desktop.png')
            page.set_viewport_size({"width":390,"height":844})
            page.get_by_role('button',name='Session controls',exact=True).click()
            page.get_by_label('Codex thread UUID',exact=True).scroll_into_view_if_needed()
            expect(page.get_by_label('Codex thread UUID',exact=True)).to_be_in_viewport()
            page.get_by_label('Demodex session UUID',exact=True).scroll_into_view_if_needed()
            expect(page.get_by_label('Demodex session UUID',exact=True)).to_be_in_viewport()
            assert page.evaluate('document.body.scrollWidth <= innerWidth')
            page.screenshot(path='/tmp/demodex-overview-mobile.png')
            page.get_by_role('button',name='Close',exact=True).click()
            page.get_by_role('button',name='← Sessions',exact=True).click()
            expect(page.get_by_role('navigation',name='Sessions by project')).to_be_visible()
            expect(page.locator('.tree-folder > .folder-name')).to_contain_text('/home/operator/src/cairn-next')
            page.screenshot(path='/tmp/demodex-overview-tree.png')
            page.locator('.session').filter(has_text='Wormhole fixture').click()
            expect(page.get_by_label('Composer model',exact=True)).to_be_visible()
            expect(page.get_by_label('Composer model',exact=True)).to_be_enabled()
            assert page.evaluate('document.body.scrollWidth <= innerWidth')
            prompts_before=sum(c.get('method')=='turn/start' for c in codex.calls)
            page.get_by_label('Message',exact=True).fill('/model')
            page.get_by_role('button',name='Send',exact=True).click()
            controls=page.get_by_role('region',name='Session controls',exact=True)
            expect(controls).to_be_visible()
            expect(controls.get_by_label('Model',exact=True)).to_be_enabled()
            expect(controls.locator('.model-controls .control-warning')).to_have_count(0)
            expect(controls.locator('.model-description')).to_be_visible()
            expect(controls.locator('.model-description')).to_contain_text('Fixture model')
            controls.get_by_label('Model',exact=True).select_option('fixture-next')
            expect(controls.get_by_label('Reasoning effort',exact=True)).to_have_value('future-level')
            controls.get_by_label('Service tier',exact=True).select_option('priority')
            controls.get_by_role('button',name='Apply model',exact=True).click()
            expect(controls.locator('.accepted-model')).to_have_text('Accepted model: fixture-next · future-level · priority')
            expect(page.get_by_label('Composer model',exact=True)).to_have_value('fixture-next')
            expect(controls.get_by_label('Model',exact=True)).to_have_value('fixture-next')
            expect(controls.get_by_label('Reasoning effort',exact=True)).to_have_value('future-level')
            controls.get_by_label('Goal objective',exact=True).fill('Finish the operator UI')
            controls.get_by_label('Token budget',exact=False).fill('5000')
            controls.get_by_role('button',name='Save paused',exact=True).click()
            expect(controls.locator('.goal-status')).to_have_text('Goal: paused')
            assert codex.goal['tokenBudget']==5000
            expect(page.locator('aside .goal-indicator')).to_contain_text('Goal · paused')
            controls.get_by_label('Goal objective',exact=True).fill('Keep unsaved goal edits')
            controls.get_by_label('Token budget',exact=False).fill('7000')
            controls.get_by_role('button',name='Start / resume goal',exact=True).click()
            expect(controls.locator('.goal-status')).to_have_text('Goal: active')
            codex.send('turn/started',{'turn':{'id':'turn'}})
            expect(controls.get_by_role('button',name='Apply model',exact=True)).to_be_disabled()
            controls.get_by_role('button',name='Pause goal',exact=True).click()
            expect(controls.locator('.goal-status')).to_have_text('Goal: paused')
            assert codex.goal['objective']=='Finish the operator UI' and codex.goal['tokensUsed']==123
            expect(controls.get_by_label('Goal objective',exact=True)).to_have_value('Keep unsaved goal edits')
            expect(controls.get_by_label('Token budget',exact=False)).to_have_value('7000')
            codex.send('turn/completed',{'turn':{'id':'turn'}})
            controls.get_by_role('button',name='Mark goal complete',exact=True).click()
            expect(controls.locator('.goal-status')).to_have_text('Goal: complete')
            expect(controls.get_by_label('Model',exact=True)).to_have_value('fixture-next')
            page.locator('.transcript').evaluate('e=>e.scrollTop=0')
            page.screenshot(path='/tmp/demodex-controls-mobile.png')
            controls.get_by_role('button',name='Clear goal',exact=True).click()
            expect(controls.locator('.goal-status')).to_have_text('No goal set')
            page.get_by_role('button',name='Close',exact=True).click()
            page.get_by_role('button',name='Session controls',exact=True).click()
            codex.send('turn/started',{'turn':{'id':'turn'}})
            expect(controls.get_by_role('button',name='Apply model',exact=True)).to_be_disabled()
            controls.get_by_label('Goal objective',exact=True).fill('Set during active work')
            expect(controls.get_by_role('button',name='Start goal',exact=True)).to_be_enabled()
            controls.get_by_role('button',name='Start goal',exact=True).click()
            expect(controls.locator('.goal-status')).to_have_text('Goal: active')
            assert codex.goal['objective']=='Set during active work'
            assert sum(c.get('method')=='turn/start' for c in codex.calls)==prompts_before
            controls.get_by_label('Goal objective',exact=True).fill('Replace during active work')
            expect(controls.get_by_text('Replacing the objective resets goal usage accounting.',exact=True)).to_be_visible()
            controls.get_by_role('button',name='Save paused',exact=True).click()
            expect(controls.locator('.goal-status')).to_have_text('Goal: paused')
            assert codex.goal['objective']=='Replace during active work'
            codex.send('turn/completed',{'turn':{'id':'turn'}})
            controls.get_by_role('button',name='Clear goal',exact=True).click()
            expect(controls.locator('.goal-status')).to_have_text('No goal set')
            page.get_by_role('dialog',name='Session controls',exact=True).get_by_role('button',name='Close',exact=True).click()
            page.get_by_label('Message',exact=True).fill('/goal Another explicit objective')
            page.get_by_role('button',name='Send',exact=True).click()
            expect(controls.get_by_label('Goal objective',exact=True)).to_have_value('Another explicit objective')
            assert codex.goal is None, 'opening the goal form created a goal'
            page.get_by_role('button',name='Close',exact=True).click()
            page.get_by_label('Message',exact=True).fill('/unsupported-command')
            page.get_by_role('button',name='Send',exact=True).click()
            expect(page.get_by_role('alert')).to_contain_text('Nothing was sent to Codex')
            assert sum(c.get('method')=='turn/start' for c in codex.calls)==prompts_before
            page.get_by_role('button',name='Dismiss',exact=True).click()
            page.get_by_role('button',name='Session controls',exact=True).click()
            archive=page.get_by_role('button',name='Archive session',exact=True)
            codex.send('turn/started',{'turn':{'id':'turn'}})
            expect(archive).to_be_disabled()
            interrupts=sum(c.get('method')=='turn/interrupt' for c in codex.calls)
            page.get_by_role('button',name='Close',exact=True).click()
            page.get_by_role('button',name='Interrupt',exact=True).click()
            page.get_by_role('button',name='Session controls',exact=True).click()
            expect(archive).to_be_enabled()
            assert sum(c.get('method')=='turn/interrupt' for c in codex.calls)==interrupts+1
            page.get_by_role('button',name='Close',exact=True).click()
            page.set_viewport_size({'width':1200,'height':850})
            page.locator('aside > nav .session-archive-action').click()
            expect(page.locator('.archived-sessions summary')).to_have_text('Archived sessions (1)')
            expect(page.locator('aside > nav .session')).to_have_count(0)
            assert api('/sessions')[0]['archived'] is True
            page.reload()
            page.locator('.archived-sessions summary').click()
            page.locator('.archived-sessions .session').click()
            expect(page.get_by_role('button',name='Send',exact=True)).to_be_disabled()
            expect(page.get_by_label('Message',exact=True)).to_have_value('/unsupported-command')
            expect(page.locator('.session-heading .agent-name')).to_contain_text(identity)
            expect(page.locator('.session-heading h1')).to_have_text('Wormhole fixture')
            # Closing is navigation, including archived sessions: persist the
            # blank view, preserve drafts, and support browser back/forward.
            page.get_by_role('button',name='Close session view',exact=True).click()
            expect(page.locator('.session-heading')).to_have_count(0)
            assert json.loads(page.evaluate('history.state'))['selected']==''
            page.go_back()
            expect(page.get_by_label('Message',exact=True)).to_have_value('/unsupported-command')
            page.go_forward()
            expect(page.locator('.session-heading')).to_have_count(0)
            page.reload()
            expect(page.locator('header .indicator')).to_have_text('connected to')
            expect(page.locator('main .empty')).to_be_visible()
            expect(page.get_by_label('Message',exact=True)).to_have_count(0)
            page.locator('.archived-sessions summary').click()
            page.locator('.archived-sessions .session').click()
            expect(page.get_by_label('Message',exact=True)).to_have_value('/unsupported-command')
            page.get_by_role('button',name='Session controls',exact=True).click()
            page.get_by_role('button',name='Restore session',exact=True).click()
            expect(archive).to_be_enabled()
            assert api('/sessions')[0]['archived'] is False
            assert sum(c.get('method')=='turn/interrupt' for c in codex.calls)==interrupts+1, 'Archive/restore interrupted the agent'
            page.get_by_role('button',name='Close',exact=True).click()
            codex.send('turn/started',{'turn':{'id':'turn'}})
            expect(page.locator('.session-heading .status')).to_have_text('working')
            starts_before=sum(c['method']=='turn/start' for c in codex.calls)
            page.set_viewport_size({'width':390,'height':844})
            expect(page.get_by_role('button',name='Close session view',exact=True)).to_be_in_viewport()
            page.get_by_role('button',name='Close session view',exact=True).click()
            expect(page.get_by_label('Message',exact=True)).to_have_count(0)
            assert codex.active
            assert sum(c.get('method')=='turn/interrupt' for c in codex.calls)==interrupts+1
            page.locator('button.session').filter(has_text='Wormhole fixture').click()
            expect(page.get_by_label('Message',exact=True)).to_have_value('/unsupported-command')
            page.set_viewport_size({'width':1200,'height':850})
            # Background terminals remain visible independent of the transcript.
            codex.background = [
                {'processId':'101','itemId':'bg-one','command':'sleep 1000','cwd':'/remote/work'},
                {'processId':'102','itemId':'bg-two','command':'watch build','cwd':'/another/work'},
            ]
            codex.send('thread/status/changed',{'status':{'type':'active'}})
            expect(page.get_by_role('button',name='Background terminals (2)',exact=True)).to_be_visible()
            page.get_by_role('button',name='Background terminals (2)',exact=True).click()
            background = page.get_by_role('dialog',name='Background terminals',exact=True)
            expect(background.get_by_text('Running · Machine unknown',exact=True)).to_have_count(2)
            def child_activity(agent,kind):
                codex.send('item/completed',{'threadId':'thread','turnId':'turn','item':{'id':f'child-{agent}-{kind}','type':'subAgentActivity','agentThreadId':agent,'agentPath':f'/root/{agent}','kind':kind}})
            child_activity('one','started')
            child_activity('two','started')
            expect(page.locator('aside .subagent-count')).to_have_text('2 active subagents',timeout=15000)
            child_activity('one','completed')
            expect(page.locator('aside .subagent-count')).to_have_text('1 active subagent',timeout=15000)
            child_activity('two','interrupted')
            expect(page.locator('aside .subagent-count')).to_have_count(0,timeout=15000)

            expect(background.get_by_text('/remote/work',exact=True)).to_be_visible()
            background.get_by_role('button',name='Stop terminal',exact=True).first.click()
            expect(background.locator('.background-terminal')).to_have_count(1)
            background.get_by_role('button',name='Stop all 1 listed terminals',exact=True).click()
            expect(background.get_by_text('No background terminals running.',exact=True)).to_be_visible()
            background.get_by_role('button',name='Close',exact=True).click()
            expect(page.get_by_role('button',name='Background terminals (0)',exact=True)).to_be_visible()
            assert [c['params']['processId'] for c in codex.calls if c.get('method')=='thread/backgroundTerminals/terminate']==['101','102']
            composer.fill('Finish-race fixture')
            page.get_by_role('button',name='Send',exact=True).click()
            expect(composer).to_have_value('')
            assert sum(c['method']=='turn/start' for c in codex.calls)==starts_before+1
            race_calls=[c for c in codex.calls if c.get('params',{}).get('input',[{}])[0].get('text')=='Finish-race fixture']
            assert [c['method'] for c in race_calls]==['turn/steer','turn/start']
            assert race_calls[0]['params']['clientUserMessageId']==race_calls[1]['params']['clientUserMessageId']
            assert not codex.queue
            # Human-readable streaming activity, tool outcomes and errors.
            codex.send('item/started',{'item':{'id':'ui-reasoning','type':'reasoning','summary':[]}})
            codex.send('item/reasoning/summaryTextDelta',{'itemId':'ui-reasoning','summaryIndex':0,'delta':'Checking the UI controls'})
            reasoning_group = page.locator('.tool-group').last
            expect(reasoning_group.locator(':scope > summary')).to_contain_text('Reasoning')
            if reasoning_group.get_attribute('open') is None:
                reasoning_group.locator(':scope > summary').click()
            expect(page.locator('[data-item-id="ui-reasoning"]')).to_contain_text('Checking the UI controls')
            codex.send('turn/plan/updated',{'turnId':'turn','plan':[{'step':'Inspect controls','status':'completed'},{'step':'Check activity','status':'inProgress'}]})
            expect(page.locator('.plan-steps')).to_contain_text('In progressCheck activity')
            codex.send('item/started',{'item':{'id':'ui-command','type':'commandExecution','command':'cargo test','cwd':'/workspace','status':'inProgress'}})
            group=page.locator('.tool-group[data-group-id="ui-command"]')
            expect(group).not_to_have_attribute('open','')
            expect(page.locator('[data-item-id="ui-command"]')).to_have_count(0)
            group.locator(':scope > summary').click()
            command=page.locator('[data-item-id="ui-command"]')
            expect(command).to_contain_text('Running')
            codex.send('item/completed',{'item':{'id':'ui-command','type':'commandExecution','command':'cargo test','status':'completed','exitCode':1,'durationMs':1200,'aggregatedOutput':'Test failure'}})
            expect(command.locator('.item-status.failed')).to_contain_text('exit 1')
            expect(command.locator('details')).not_to_have_attribute('open','')
            group=page.locator('.tool-group[data-group-id="ui-command"]')
            expect(group).to_have_attribute('open','')
            expect(group.locator(':scope > summary')).to_contain_text('1 failed')
            command.locator('summary').click()
            expect(command).to_contain_text('Test failure')
            codex.send('item/started',{'item':{'id':'ui-tool','type':'mcpToolCall','server':'docs','tool':'search','status':'inProgress'}})
            codex.send('item/mcpToolCall/progress',{'itemId':'ui-tool','message':'Searching documentation'})
            expect(page.locator('[data-item-id="ui-tool"]')).to_contain_text('Searching documentation')
            codex.send('item/completed',{'item':{'id':'ui-files','type':'fileChange','status':'completed','changes':[{'path':'src/main.rs','kind':{'type':'update'},'diff':'+ changed'}]}})
            expect(page.locator('[data-item-id="ui-files"]')).to_contain_text('src/main.rs')
            expect(group).to_have_attribute('open','')
            expect(command.locator('details')).to_have_attribute('open','')
            page.locator('[data-item-id="ui-files"] summary').click()
            expect(page.locator('.diff-add')).to_contain_text('+ changed')
            group.locator(':scope > summary').click()
            expect(group.locator(':scope > summary')).to_contain_text('failed')
            codex.send('error',{'turnId':'turn','error':{'message':'Temporary upstream failure'},'willRetry':True})
            expect(page.locator('.conversation p.error')).to_contain_text('Codex scheduled a retry')
            codex.send('turn/completed',{'turn':{'id':'turn','status':'failed','error':{'message':'Upstream unavailable'}}})
            expect(page.locator('[data-item-id="demodex:turn:turn"]')).to_contain_text('Upstream unavailable')
            latest=page.get_by_role('button',name='Jump to latest',exact=True)
            if latest.count(): latest.click()
            page.screenshot(path=str(ROOT/'target'/'review-activity-desktop.png'))
            page.set_viewport_size({'width':390,'height':844})
            page.wait_for_function('()=>{const e=document.querySelector(".transcript");return e.scrollHeight-e.clientHeight-e.scrollTop<=1}')
            expect(page.locator('[data-item-id="ui-tool"]')).to_contain_text('Turn ended · outcome unreported')
            expect(page.locator('.activity')).to_have_count(0)
            page.screenshot(path=str(ROOT/'target'/'review-activity-mobile.png'))
            # Only requests whose owning Codex connection ended can be hidden.
            # Dismissal survives reads/reloads in this tab and never answers Codex.
            codex.send('item/fileChange/requestApproval', {
                'threadId': 'thread', 'turnId': 'turn', 'itemId': 'lost-approval',
                'reason': 'Unavailable approval fixture',
            }, 899)
            lost = page.get_by_role('region', name='Approve file changes', exact=True)
            expect(lost).to_contain_text('Waiting for you')
            expect(lost.get_by_role('button', name='Dismiss', exact=True)).to_have_count(0)
            answers_before = list(codex.answers)
            interrupts_before = sum(c.get('method') == 'turn/interrupt' for c in codex.calls)
            codex.socket.close()
            expect(lost).to_contain_text('Unavailable')
            expect(lost.get_by_role('button', name='Approve once', exact=True)).to_be_disabled()
            lost.get_by_role('button', name='Dismiss', exact=True).click()
            expect(lost).to_have_count(0)
            page.evaluate("window.dispatchEvent(new Event('online'))")
            expect(lost).to_have_count(0)
            page.reload()
            expect(page.get_by_label('Message', exact=True)).to_be_visible()
            expect(lost).to_have_count(0)
            stored = api(f'/sessions/{session_id}')['pending']
            assert any(p['state'] == 'unavailable' and p['params'].get('itemId') == 'lost-approval' for p in stored)
            assert codex.answers == answers_before
            assert sum(c.get('method') == 'turn/interrupt' for c in codex.calls) == interrupts_before
            assert not rest_requests, rest_requests
            assert not errors, errors
            browser.close()
        print("PASS: Wormhole auth, commands/events, mobile frame, pending questions, reconnect, drafts, PWA update, switching independent hosts, no browser REST")
    except Exception:
        try: page.screenshot(path='/tmp/demodex-controls-failure.png')
        except Exception: pass
        log.flush()
        log.seek(0)
        print(log.read()[-12000:])
        raise
    finally:
        for process in reversed(processes):
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        server.shutdown()
        incompatible_server.shutdown()
