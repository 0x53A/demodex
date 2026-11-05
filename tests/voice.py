# /// script
# dependencies = ["playwright", "websockets>=15"]
# ///
"""Browser capture, transcript tags and per-instance archives; synthetic STT, no inference."""
import json, os, socket, subprocess, tempfile, threading, time, sys, sqlite3, re
from pathlib import Path
from playwright.sync_api import sync_playwright, expect
from websockets.sync.server import serve
from websockets.exceptions import ConnectionClosed
from wormhole_client import api
ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('DEMODEX_BIN', ROOT/'target/rust-pwa/debug/demodex'))
DIST = Path(os.environ.get('DEMODEX_WEB_DIST', ROOT/'web/.voice-dist'))
def port():
    with socket.socket() as s: s.bind(('127.0.0.1',0)); return s.getsockname()[1]
def wait_port(n):
    for _ in range(200):
        try:
            with socket.create_connection(('127.0.0.1',n),timeout=.1): return
        except OSError: time.sleep(.05)
    raise AssertionError('service did not start')
class Codex:
    def handle(self, ws):
        try:
            for raw in ws:
                c=json.loads(raw)
                if 'id' not in c: continue
                m=c['method']; r={}
                if m in ['thread/start','thread/resume']: r={'thread':{'id':'thread','turns':[]},'sandbox':{'type':'readOnly'}}
                elif m=='thread/read': r={'thread':{'id':'thread','status':{'type':'idle'}}}
                elif m in ['thread/queue/list','thread/backgroundTerminals/list','model/list']: r={'data':[],'nextCursor':None}
                elif m=='thread/goal/get': r={'goal':None}
                assert m != 'turn/start', 'Voice must not send a turn'
                ws.send(json.dumps({'id':c['id'],'result':r}))
        except ConnectionClosed: pass
with tempfile.TemporaryDirectory(prefix='demodex-voice-') as temp:
    d=Path(temp); dp,wp,cp=port(),port(),port(); host=f'http://127.0.0.1:{dp}'; origin=f'http://127.0.0.1:{wp}'
    mock=d/'ssh'; audit=d/'audit'; mode=d/'mode'; mode.write_text('normal')
    mock.write_text(f'#!{sys.executable}\n'+r'''
import sys,struct,json,pathlib,time
mode=pathlib.Path(__file__).with_name('mode').read_text()
def read():
    h=sys.stdin.buffer.read(4)
    if len(h)!=4: raise EOFError()
    return json.loads(sys.stdin.buffer.read(struct.unpack('>I',h)[0]))
def write(x):
    b=json.dumps(x).encode();sys.stdout.buffer.write(struct.pack('>I',len(b))+b);sys.stdout.buffer.flush()
n=0
try:
    assert '-o' in sys.argv and 'StrictHostKeyChecking=yes' in sys.argv and 'BatchMode=yes' in sys.argv
    first=read();assert first['Start']['version']==1
    if mode=='fail': write({'Error':'STT fixture unavailable'});sys.exit(0)
    write({'Ready':{'model':'Synthetic fixture'}})
    while True:
        request=read()
        if request=='Finish':
            assert n>0
            if mode=='slow':time.sleep(2)
            write({'Text':'Synthetic voice transcript.'});break
        chunk=request['Audio'];assert chunk['sequence']==n and 0<len(chunk['pcm'])<=8000;n+=1
        if mode=='drop':sys.exit(0)
        write('Ack')
except EOFError: pass
finally:
    with pathlib.Path(__file__).with_name('audit').open('a') as f:f.write(str(n)+'\n')
''');mock.chmod(0o700)
    config=d/'stt.json';config.write_text(json.dumps({'destination':'fixture','socket':'/run/stt.sock','program':'/bin/demodex-stt'}))
    server=serve(Codex().handle,'127.0.0.1',cp);threading.Thread(target=server.serve_forever,daemon=True).start()
    processes=[]; log=(d/'log').open('w+')
    try:
        env=os.environ|{'PATH':str(d)+':'+os.environ['PATH']}
        for args in [('--bind',f'127.0.0.1:{dp}','--data-dir',str(d/'state'),'--api-only','--allowed-origin',origin,'--stt-config',str(config)),('web','--bind',f'127.0.0.1:{wp}','--directory',str(DIST))]:
            processes.append(subprocess.Popen([str(BINARY),*args],cwd=ROOT,stdout=log,stderr=log,env=env))
        wait_port(dp);wait_port(wp);token=(d/'state/access-token').read_text().strip()
        api(host,token,'/sessions',{'name':'Voice fixture','endpoint':f'ws://127.0.0.1:{cp}'})
        with sync_playwright() as pw:
            browser=pw.chromium.launch(executable_path=os.environ.get('CHROME','/run/current-system/sw/bin/google-chrome'),headless=True,args=['--no-sandbox','--use-fake-device-for-media-stream','--use-fake-ui-for-media-stream'])
            context=browser.new_context();page=context.new_page();errors=[];page.on('pageerror',lambda e:errors.append(str(e)))
            context.add_init_script('''
              window.voiceTracks=[];
              const get=navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
              navigator.mediaDevices.getUserMedia=async opts=>{const s=await get(opts);window.voiceTracks.push(...s.getTracks());return s;};
              Object.defineProperty(navigator, 'wakeLock', {value: {
                request: async () => { throw new DOMException('Battery policy', 'NotAllowedError'); }
              }});
            ''')
            page.goto(origin);page.get_by_role('button',name='+ connection',exact=True).click()
            page.get_by_label('Host URL').fill(host);page.get_by_label('Access token').fill(token);page.get_by_role('button',name='Save and connect').click()
            expect(page.locator('header .indicator')).to_have_text('connected to',timeout=20000)
            page.get_by_role('button',name=re.compile('^Voice fixture')).click()
            page.locator('.session-error').get_by_role('button',name='Reconnect',exact=True).click()
            expect(page.locator('.session-heading .status')).to_have_text('connected')
            prompt=page.locator('#prompt');prompt.fill('Existing draft')
            record=page.get_by_role('button',name='Record voice',exact=True)
            # The split voice control stays immediately left of Send on narrow screens.
            for width in (360, 1280):
                page.set_viewport_size({'width':width,'height':900})
                mic=record.bounding_box()
                attach=page.get_by_role('button',name='Attach file',exact=True).bounding_box()
                assert abs(attach['y']-mic['y']) < 2
                assert 0 <= mic['x']-(attach['x']+attach['width']) <= 10
                language=page.get_by_role('combobox',name='Speech language').bounding_box()
                send=page.get_by_role('button',name='Send',exact=True).bounding_box()
                assert abs(mic['y']-send['y']) < 2 and abs(language['y']-send['y']) < 2
                assert abs(mic['x']+mic['width']-language['x']) < 2
                assert 0 <= send['x']-(language['x']+language['width']) <= 10
                assert send['x']+send['width'] <= width
                if os.environ.get('DEMODEX_SCREENSHOTS'):
                    page.screenshot(path=str(Path(os.environ['DEMODEX_SCREENSHOTS'])/f'composer-{width}.png'))
            page.get_by_role('combobox',name='Speech language').select_option('de')
            expect(page.get_by_role('combobox',name='Speech language')).to_have_value('de')
            record.click();expect(page.get_by_role('button',name='Stop',exact=True)).to_be_enabled(timeout=10000)
            expect(page.locator('.voice-input .error')).to_contain_text('Cannot keep the screen on')
            expect(page.get_by_role('button',name='Send',exact=True)).to_be_disabled()
            page.wait_for_timeout(1100);page.get_by_role('button',name='Stop',exact=True).click()
            expect(prompt).to_have_value('Existing draft\n<tts>\nSynthetic voice transcript.\n</tts>',timeout=10000)
            assert page.evaluate('voiceTracks.every(t=>t.readyState==="ended")')
            record.click();expect(page.get_by_role('button',name='Stop',exact=True)).to_be_enabled();page.wait_for_timeout(400)
            page.locator('.voice-input').get_by_role('button',name='Cancel',exact=True).click()
            expect(record).to_be_visible();expect(prompt).to_have_value('Existing draft\n<tts>\nSynthetic voice transcript.\n</tts>')
            assert page.evaluate('voiceTracks.every(t=>t.readyState==="ended")')
            mode.write_text('fail');record.click();expect(page.locator('.voice-input .error')).to_contain_text('STT fixture unavailable',timeout=10000)
            assert page.evaluate('voiceTracks.every(t=>t.readyState==="ended")')
            # Cancel while inference is pending; its eventual reply must not change the draft.
            mode.write_text('slow');record.click();expect(page.get_by_role('button',name='Stop',exact=True)).to_be_enabled();page.wait_for_timeout(600)
            page.get_by_role('button',name='Stop',exact=True).click();page.locator('.voice-input').get_by_role('button',name='Cancel',exact=True).click();page.wait_for_timeout(2500)
            expect(prompt).to_have_value('Existing draft\n<tts>\nSynthetic voice transcript.\n</tts>')
            assert not errors,errors
            # Navigation tears down recording; no text may land in another draft.
            mode.write_text('normal')
            api(host,token,'/sessions',{'name':'Other voice fixture','endpoint':f'ws://127.0.0.1:{cp}'})
            record.click();expect(page.get_by_role('button',name='Stop',exact=True)).to_be_enabled()
            page.locator('.session').filter(has_text='Other voice fixture').click()
            page.wait_for_timeout(400)
            assert page.evaluate('voiceTracks.every(t=>t.readyState==="ended")')
            expect(prompt).to_have_value('')
            page.get_by_role('button',name=re.compile('^Voice fixture')).click()
            expect(prompt).to_have_value('Existing draft\n<tts>\nSynthetic voice transcript.\n</tts>')
            # If the upstream dies after accepting audio, the local recording survives.
            mode.write_text('drop');record.click()
            expect(page.locator('.voice-input .error')).to_contain_text('STT SSH connection closed',timeout=10000)
            expect(prompt).to_have_value('Existing draft\n<tts>\nSynthetic voice transcript.\n</tts>')
            # Permission refusal is explicit and leaves the draft usable.
            page.evaluate("() => { navigator.mediaDevices.getUserMedia=async()=>{throw new DOMException('Microphone denied','NotAllowedError')}; }")
            record.click();expect(page.locator('.voice-input .error')).to_contain_text('Microphone denied')
            expect(page.get_by_role('button',name='Send',exact=True)).to_be_enabled()
            assert not errors,errors
            browser.close()
        # Audio and original STT output are local sidecar archives, not conversation events.
        archived=[]
        for _ in range(100):
            archived=[(p, json.loads((p/'metadata.json').read_text())) for p in (d/'state/voice-recordings').iterdir() if (p/'audio.flac').exists()]
            if len(archived)==6:break
            time.sleep(.05)
        assert len(archived)==6, archived
        assert all((p/'audio.flac').read_bytes().startswith(b'fLaC') for p,_ in archived)
        assert all((p/'audio.flac').stat().st_mode & 0o777 == 0o600 for p,_ in archived)
        assert all(m['sample_rate']==16000 and m['channels']==1 and m['session_id'] for _,m in archived)
        completed=[m for _,m in archived if m['status']=='completed']
        assert len(completed)==1 and completed[0]['transcript']=='Synthetic voice transcript.'
        assert completed[0]['samples']>0
        assert any(m['status']=='cancelled' and m['samples']>0 for _,m in archived)
        assert any(m['status']=='failed' and m['samples']>0 for _,m in archived)
        assert any(m['status']=='failed' and m['samples']==0 for _,m in archived)
        assert not list((d/'state/voice-recordings').glob('*/audio.pcm'))
        # No transcription command/audio is stored in receipts or events.
        databases=[p for p in (d/'state').iterdir() if p.is_file() and p.open('rb').read(16)==b'SQLite format 3\x00']
        assert databases
        for db in databases:
            con=sqlite3.connect(db)
            assert 'Synthetic voice transcript.' not in '\n'.join(con.iterdump())
            con.close()
        print('Voice capture, tagged drafts, cancellation/failure archives, cleanup and database separation passed')
    except Exception:
        log.flush();print((d/'log').read_text()[-8000:]);raise
    finally:
        for p in processes:p.terminate()
        for p in processes:
            try:p.wait(timeout=10)
            except subprocess.TimeoutExpired:p.kill();p.wait()
        server.shutdown();log.close()
