# /// script
# dependencies = ["playwright", "websockets>=15"]
# ///
"""Explicit notification chat and mocked browser subscription/SW routing; no real push or inference."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
from playwright.sync_api import sync_playwright, expect
from websockets.sync.server import serve
from websockets.exceptions import ConnectionClosed
from wormhole_client import api, call

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / 'target/rust-pwa/debug/demodex'
DIST = Path(os.environ.get('DEMODEX_WEB_DIST', ROOT / 'web/.rust-dist'))

def port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]

def wait_port(n):
    for _ in range(200):
        try:
            with socket.create_connection(('127.0.0.1', n), timeout=.1): return
        except OSError: time.sleep(.05)
    raise AssertionError('service did not start')

class Codex:
    socket = None
    def handle(self, ws):
        self.socket = ws
        try:
            for raw in ws:
                call = json.loads(raw)
                if 'id' not in call: continue
                method = call.get('method')
                result = {}
                if method in ('thread/start', 'thread/resume'): result = {'thread': {'id':'thread','turns':[]}, 'sandbox':{'type':'readOnly'}}
                elif method == 'thread/read': result = {'thread':{'id':'thread','status':{'type':'idle'}}}
                elif method == 'thread/queue/list': result = {'data':[], 'nextCursor':None}
                elif method == 'thread/backgroundTerminals/list': result = {'data':[], 'nextCursor':None}
                elif method == 'thread/goal/get': result = {'goal':None}
                elif method == 'model/list': result = {'data':[], 'nextCursor':None}
                ws.send(json.dumps({'id':call['id'], 'result':result}))
        except ConnectionClosed: pass
    def message(self, ident, text):
        self.socket.send(json.dumps({'method':'item/completed','params':{'threadId':'thread','item':{'id':ident,'type':'agentMessage','text':text}}}))

with tempfile.TemporaryDirectory(prefix='demodex-push-') as temp:
    directory=Path(temp)
    daemon_port,web_port,codex_port=port(),port(),port()
    host=f'http://127.0.0.1:{daemon_port}'
    origin=f'http://127.0.0.1:{web_port}'
    codex=Codex()
    server=serve(codex.handle,'127.0.0.1',codex_port)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    processes=[]
    log=(directory/'log').open('w+')
    try:
        for args in [('--bind',f'127.0.0.1:{daemon_port}','--data-dir',str(directory/'state'),'--api-only','--allowed-origin',origin),('web','--bind',f'127.0.0.1:{web_port}','--directory',str(DIST))]:
            processes.append(subprocess.Popen([str(BINARY),*args],cwd=ROOT,stdout=log,stderr=log))
        wait_port(daemon_port);wait_port(web_port)
        token=(directory/'state/access-token').read_text().strip()
        created=api(host,token,'/sessions',{'name':'Notification fixture','endpoint':f'ws://127.0.0.1:{codex_port}'})
        with sync_playwright() as pw:
            browser=pw.chromium.launch(executable_path=os.environ.get('CHROME','/run/current-system/sw/bin/google-chrome'),headless=True,args=['--no-sandbox'])
            context=browser.new_context(viewport={'width':1100,'height':850})
            # Browser API boundary only: never contact a real push provider or OS.
            context.add_init_script('''
                window.permissionCalls=0;window.subscribeCalls=0;window.pushPermission='default';
                Object.defineProperty(Notification,'permission',{get:()=>window.pushPermission});
                Notification.requestPermission=async()=>{if(!navigator.userActivation.isActive)throw Error('Permission requested without user activation');window.permissionCalls++;return window.pushPermission='granted';};
                window.fakeSubscription=null;
                PushManager.prototype.getSubscription=async()=>window.fakeSubscription;
                PushManager.prototype.subscribe=async function(options){
                    window.subscribeCalls++;
                    const pair=await crypto.subtle.generateKey({name:'ECDH',namedCurve:'P-256'},true,['deriveBits']);
                    const publicKey=new Uint8Array(await crypto.subtle.exportKey('raw',pair.publicKey));
                    const encode=(v)=>btoa(String.fromCharCode(...v)).replace(/\\+/g,'-').replace(/\\//g,'_').replace(/=/g,'');
                    const data={endpoint:'https://fcm.googleapis.com/fcm/send/fixture-no-delivery',keys:{p256dh:encode(publicKey),auth:encode(crypto.getRandomValues(new Uint8Array(16)))}};
                    return window.fakeSubscription={options,toJSON:()=>data,unsubscribe:async()=>{window.fakeSubscription=null;return true;}};
                };
            ''')
            page=context.new_page();errors=[]
            page.on('pageerror',lambda e:errors.append(str(e)))
            page.goto(origin)
            page.get_by_role('button',name='+ connection',exact=True).click()
            page.get_by_label('Host URL').fill(host);page.get_by_label('Access token').fill(token)
            page.get_by_role('button',name='Save and connect').click()
            expect(page.locator('header .indicator')).to_have_text('connected to',timeout=20000)
            page.locator('.session').filter(has_text='Notification fixture').click()
            page.locator('.session-error').get_by_role('button',name='Reconnect',exact=True).click()
            expect(page.locator('.session-heading .status')).to_have_text('connected')
            # Explicit dynamic tool call with push disabled creates one durable chat entry.
            request={'id':'notify-request','method':'item/tool/call','params':{'namespace':'demodex','tool':'notify','threadId':'thread','callId':'notify-once','arguments':{'message':'Please review the candidate.','title':'Review ready'}}}
            codex.socket.send(json.dumps(request))
            expect(page.locator('.chat-notification')).to_contain_text('Please review the candidate.')
            codex.socket.send(json.dumps(request))
            expect(page.locator('.chat-notification')).to_have_count(1)
            expect(page.locator('.chat-notification small')).to_contain_text('Notification saved here',timeout=15000)
            page.get_by_role('button',name='Server settings',exact=True).click()
            panel=page.get_by_role('region',name='Push notifications')
            expect(panel.get_by_role('button',name='Enable push on this device')).to_be_enabled(timeout=15000)
            assert page.evaluate('permissionCalls')==0
            panel.get_by_label('Hide message previews').check()
            panel.get_by_role('button',name='Enable push on this device').click()
            expect(panel.get_by_role('button',name='Send test notification')).to_be_enabled(timeout=15000)
            assert page.evaluate('permissionCalls')==1
            local=page.evaluate('demodexPush.status()')
            device=local['binding']['device_id'];server_id=local['binding']['server_id']
            remote=call(host,token,{'PushSettings':{'device_id':device}})
            assert remote['enabled'] and remote['hide_preview'],remote
            assert 'auth' not in remote and 'endpoint' not in remote
            # Worker validates local server association and builds credential-free links.
            worker=context.service_workers[0]
            worker.evaluate('''self.captured=[];self.registration.showNotification=async(title,options)=>self.captured.push({title,...options});self.opened=[];self.clients.openWindow=async(url)=>self.opened.push(url);void 0;''')
            payload={'id':'push-1','server_id':server_id,'server_url':host,'session_id':created['id'],'title':'Fixture notification','message':'Fixture only'}
            worker.evaluate('(p)=>self.dispatchEvent(new PushEvent("push",{data:JSON.stringify(p)}))',payload)
            for _ in range(100):
                captures=worker.evaluate('self.captured')
                if captures:break
                time.sleep(.05)
            assert len(captures)==1,captures
            assert token not in captures[0]['data']['url']
            assert 'notify_session='+created['id'] in captures[0]['data']['url']
            worker.evaluate('(p)=>{const event=new Event("notificationclick");event.notification={data:p,close(){}};event.waitUntil=(v)=>v;self.dispatchEvent(event);}',captures[0]['data'])
            assert worker.evaluate('self.opened')[0]==captures[0]['data']['url'], (worker.evaluate('self.opened'),captures[0])
            other={**payload,'id':'foreign','server_id':'another-server'}
            worker.evaluate('(p)=>self.dispatchEvent(new PushEvent("push",{data:JSON.stringify(p)}))',other)
            time.sleep(.2);assert len(worker.evaluate('self.captured'))==1
            # Disable invalidates the actual browser subscription and remote registration.
            panel.get_by_role('button',name='Disable push',exact=True).click()
            expect(panel.get_by_role('button',name='Send test notification')).to_be_disabled()
            expect(panel.get_by_role('button',name='Disable push',exact=True)).to_have_count(0)
            assert call(host,token,{'PushSettings':{'device_id':device}})['enabled'] is False
            # Clicking from another selected server preserves its forms and chat drafts.
            page.evaluate("""()=>{const saved=JSON.parse(sessionStorage.getItem('demodex-rust-view'));saved.host='https://other.example';saved.fields={new_session_name:'Keep other form'};saved.drafts['https://other.example:other-session']='Keep other draft';sessionStorage.setItem('demodex-rust-view',JSON.stringify(saved));}""")
            page.goto(captures[0]['data']['url'])
            expect(page.locator('.session-heading')).to_contain_text('Notification fixture',timeout=20000)
            assert 'notify_server' not in page.url
            saved=page.evaluate("JSON.parse(sessionStorage.getItem('demodex-rust-view'))")
            assert saved['host_fields']['https://other.example']['new_session_name']=='Keep other form'
            assert saved['drafts']['https://other.example:other-session']=='Keep other draft'
            page.set_viewport_size({'width':390,'height':844})
            page.get_by_role('button',name='Server settings',exact=True).click()
            page.get_by_role('region',name='Push notifications').scroll_into_view_if_needed()
            assert page.evaluate('document.documentElement.scrollWidth<=innerWidth')
            page.screenshot(path=str(ROOT/'target/notifications-mobile.png'))
            assert not errors,errors
            browser.close()
        print('PASS: explicit durable notification, duplicate receipt, permission gesture, subscription privacy/settings/removal, mocked SW delivery/click/server binding, mobile layout; no real push sent')
    finally:
        for process in reversed(processes):
            process.terminate()
            try:process.wait(timeout=5)
            except subprocess.TimeoutExpired:process.kill();process.wait()
        server.shutdown()
