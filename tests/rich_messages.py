# /// script
# dependencies = ["playwright", "websockets>=15"]
# ///
"""Markdown, native MathML and sandboxed local Mermaid; no inference."""
import json
import base64
import hashlib
import sqlite3
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
BINARY = Path(os.environ.get('DEMODEX_BIN', ROOT / 'target/rust-pwa/debug/demodex'))
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


def expect_diagram(article):
    frame = article.locator('iframe')
    expect(frame).to_have_attribute('data-render-state', 'complete', timeout=30000)
    diagram = article.frame_locator('iframe').locator('#diagram')
    expect(diagram).to_have_attribute('data-render-state', 'complete')
    expect(diagram).not_to_contain_text('Rendering diagram')
    expect(diagram.locator('svg')).to_contain_text('Start')
    expect(diagram.locator('svg')).to_contain_text('Finish')
    expect(diagram.locator('svg')).to_be_visible()
    expect(diagram.locator('a,img,image,[href],[src],[srcset]')).to_have_count(0)
    dimensions = diagram.evaluate('(e)=>({height:e.scrollHeight,frame:innerHeight,svg:e.querySelector("svg").getBoundingClientRect().height})')
    assert dimensions['svg'] > 30, dimensions
    assert dimensions['height'] <= dimensions['frame'] + 1, dimensions

def expect_svg_image(image):
    expect(image).to_be_visible()
    assert image.get_attribute('src').startswith('data:image/svg+xml;base64,')
    image.evaluate("e => e.decode()")
    assert image.evaluate("e => e.naturalWidth > 0 && e.naturalHeight > 0")

def expect_svg_motion(image):
    expect_svg_image(image)
    # Read rendered frames: canvas drawImage can snapshot SVG's initial frame.
    first = image.screenshot(animations='allow')
    for _ in range(8):
        image.page.wait_for_timeout(150)
        if image.screenshot(animations='allow') != first:
            return
    raise AssertionError('SVG image decoded but did not animate')

class Codex:
    socket = None
    submissions = 0
    def handle(self, ws):
        self.socket = ws
        try:
            for raw in ws:
                call = json.loads(raw)
                if 'id' not in call: continue
                method = call.get('method')
                if method in ('turn/start', 'turn/steer', 'thread/queue/add'): self.submissions += 1
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

with tempfile.TemporaryDirectory(prefix='demodex-rich-') as temp:
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
        executor_port=port()
        report=directory/'status.json'
        report.write_text('{"status":"ready","count":2}\n')
        processes.append(subprocess.Popen(['codex','exec-server','--listen',f'ws://127.0.0.1:{executor_port}'],env=os.environ|{'CODEX_HOME':str(directory/'profile')},stdout=log,stderr=log))
        wait_port(executor_port)
        for args in [('--bind',f'127.0.0.1:{daemon_port}','--data-dir',str(directory/'state'),'--api-only','--allowed-origin',origin),('web','--bind',f'127.0.0.1:{web_port}','--directory',str(DIST))]:
            processes.append(subprocess.Popen([str(BINARY),*args],cwd=ROOT,stdout=log,stderr=log))
        wait_port(daemon_port);wait_port(web_port)
        token=(directory/'state/access-token').read_text().strip()
        created=api(host,token,'/sessions',{'name':'Rich fixture','endpoint':f'ws://127.0.0.1:{codex_port}','targets':[{'id':'fixture-files','url':f'ws://127.0.0.1:{executor_port}','cwd':str(directory)}]})
        with sync_playwright() as pw:
            browser=pw.chromium.launch(executable_path=os.environ.get('CHROME','/run/current-system/sw/bin/google-chrome'),headless=True,args=['--no-sandbox'])
            context=browser.new_context(viewport={'width':1100,'height':850},permissions=['clipboard-read','clipboard-write'])
            page=context.new_page();errors=[];external=[]
            page.on('pageerror',lambda e:errors.append(str(e)))
            page.on('request',lambda r:external.append(r.url) if r.url.startswith(('http:','https:')) and not r.url.startswith((host,origin)) else None)
            page.goto(origin)
            page.get_by_role('button',name='+ connection',exact=True).click()
            page.get_by_label('Host URL').fill(host);page.get_by_label('Access token').fill(token)
            page.get_by_role('button',name='Save and connect').click()
            expect(page.locator('header .indicator')).to_have_text('connected to',timeout=20000)
            page.locator('.session').filter(has_text='Rich fixture').click()
            page.locator('.session-error').get_by_role('button',name='Reconnect',exact=True).click()
            expect(page.locator('.session-heading .status')).to_have_text('connected')
            submissions_before = codex.submissions
            button_source = '<button tag="apply-fix">Yes</button> <button tag="explain">Yes</button>'
            codex.message('buttons', button_source)
            buttons = page.locator('[data-item-id="buttons"] .message-action')
            expect(buttons).to_have_count(2)
            prompt = page.locator('#prompt')
            prompt.fill('😀existing draft')
            prompt.evaluate('(e)=>{e.focus(); e.setSelectionRange(2,10)}')
            buttons.first.click()
            inserted = "(button 'apply-fix' with label 'Yes' pressed)"
            expect(prompt).to_have_value('😀 ' + inserted + ' existing draft')
            expect(prompt).to_be_focused()
            buttons.nth(1).focus()
            page.keyboard.press('Enter')
            inserted2 = "(button 'explain' with label 'Yes' pressed)"
            expect(prompt).to_have_value('😀 ' + inserted + ' ' + inserted2 + ' existing draft')
            page.reload()
            expect(prompt).to_have_value('😀 ' + inserted + ' ' + inserted2 + ' existing draft')
            assert codex.submissions == submissions_before, 'Button submitted a turn'
            prompt.fill('')
            codex.message('button-literals', '`<button tag="code">Code</button>`\n\n```html\n<button tag="fenced">Fenced</button>\n```\n\n<button tag="bad" onclick="alert(1)">Unsafe</button>')
            literals = page.locator('[data-item-id="button-literals"]')
            expect(literals).to_contain_text('Unsafe')
            expect(literals.locator('.message-action')).to_have_count(0)
            page.locator('[data-item-id="buttons"]').get_by_label('Format Markdown', exact=True).uncheck()
            expect(page.locator('[data-item-id="buttons"] .rich-message > pre')).to_have_text(button_source)
            source=r'''# Rich message

**Strong** and *emphasis*, ~~deleted~~, `code`.

| Quantity | Value |
| --- | --- |
| Energy | $E=mc^2$ |

- [x] Complete
- [ ] Pending

Inline \(\frac{x}{2}\) and display:

\[\int_0^1 x^2\,dx=\frac{1}{3}\]

```rust
let value = "<script>literal</script>";
```

[Documentation](https://example.org/docs) and [Email](mailto:person@example.org)

<https://example.org/autolink>

<script>window.renderingInjected=true</script>

[Unsafe](javascript:alert(1))

![Do not fetch](https://example.org/private.png)

```mermaid
graph LR
  A[Start] --> B[Finish]
```
'''
            codex.message('rich',source)
            article=page.locator('[data-item-id="rich"]')
            expect(article.locator('h1')).to_have_text('Rich message')
            expect(article.locator('table')).to_have_count(1)
            expect(article.locator('math')).to_have_count(3)
            assert article.locator('math').first.evaluate('(e)=>e.namespaceURI')=='http://www.w3.org/1998/Math/MathML'
            expect(article.locator('mfrac')).to_have_count(2)
            expect(article.locator('a,[href],[src],[srcset],img,image')).to_have_count(0)
            expect(article.locator('.markdown-link')).to_contain_text(['Documentation','Email','https://example.org/autolink','Unsafe'])
            expect(article.locator('.markdown-link').filter(has_text='Documentation')).to_have_attribute('title','https://example.org/docs')
            expect(article.locator('.markdown-link').filter(has_text='Email')).to_have_attribute('title','mailto:person@example.org')
            expect(article.locator('.markdown-link').filter(has_text='Unsafe')).to_have_attribute('title','javascript:alert(1)')
            expect(article.get_by_text('Open image',exact=True)).to_have_count(0)
            expect(article.locator('img,script')).to_have_count(0)
            assert page.evaluate('window.renderingInjected') is None
            expect(article.locator('pre.code-block')).to_contain_text('<script>literal</script>')
            diagram=article.frame_locator('iframe')
            expect_diagram(article)
            assert article.locator('iframe').get_attribute('sandbox')=='allow-scripts'
            assert not external,external
            image_button=article.get_by_role('button',name='Preview Do not fetch',exact=True)
            expect(image_button.locator('.image-warning')).to_be_visible()
            image_button.click()
            popup=page.get_by_role('dialog')
            expect(popup).to_contain_text('https://example.org/private.png')
            expect(popup.locator('img')).to_have_count(0)
            assert not external,external
            page.route('https://example.org/private.png', lambda route: route.fulfill(content_type='image/png',body=base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=')))
            popup.get_by_role('button',name='Load image',exact=True).click()
            expect(popup.locator('img')).to_have_attribute('src','https://example.org/private.png')
            expect(popup.locator('img')).to_have_attribute('referrerpolicy','no-referrer')
            expect(popup.locator('img')).to_be_visible()
            page.wait_for_function('document.querySelector("dialog img").naturalWidth > 0')
            popup.get_by_role('button',name='Close',exact=True).click()
            image_button.click()
            expect(page.get_by_role('dialog').locator('img')).to_have_count(0)
            page.get_by_role('dialog').get_by_role('button',name='Close',exact=True).click()
            codex.message('changing-image','![Changing](https://example.org/private.png)')
            changing=page.locator('[data-item-id="changing-image"]')
            changing.get_by_role('button',name='Preview Changing',exact=True).click()
            page.get_by_role('dialog').get_by_role('button',name='Load image',exact=True).click()
            expect(page.get_by_role('dialog').locator('img')).to_have_count(1)
            codex.message('changing-image','![Changing](https://example.org/replacement.png)')
            expect(page.get_by_role('dialog')).to_have_count(0)
            changing.get_by_role('button',name='Preview Changing',exact=True).click()
            expect(page.get_by_role('dialog').locator('img')).to_have_count(0)
            assert 'https://example.org/replacement.png' not in external
            page.get_by_role('dialog').get_by_role('button',name='Close',exact=True).click()
            external.clear()
            # A durable upload record with a real executor exercises sent-message
            # previews without a model turn; host.py covers actual upload creation.
            png=base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=')
            uploaded=directory/'uploaded.png'
            uploaded.write_bytes(png)
            target=api(host,token,'/sessions/'+created['id'])['session']['targets'][0]
            with sqlite3.connect(directory/'state/state.sqlite') as db:
                db.execute('INSERT INTO uploaded_images VALUES(?,?,?,?)',(created['id'],str(uploaded),json.dumps(target),hashlib.sha256(png).hexdigest()))
            codex.socket.send(json.dumps({'method':'item/completed','params':{'threadId':'thread','item':{'id':'sent-image','type':'userMessage','content':[{'type':'text','text':f'![Screenshot](<{uploaded}>)'}]}}}))
            sent=page.locator('[data-item-id="sent-image"]')
            expect(sent.locator('.image-thumbnail')).to_have_count(1,timeout=20000)
            sent.get_by_role('button',name='Preview Screenshot',exact=True).click()
            expect(page.get_by_role('dialog').locator('img')).to_be_visible()
            page.get_by_role('dialog').get_by_role('button',name='Close',exact=True).click()
            page.reload()
            expect(sent.locator('.image-thumbnail')).to_have_count(1,timeout=20000)
            assert call(host,token,{'ReadUploadedImage':{'id':created['id'],'path':str(uploaded)}})['dataBase64'] == base64.b64encode(png).decode()
            uploaded.write_bytes(png+b'changed')
            try:
                call(host,token,{'ReadUploadedImage':{'id':created['id'],'path':str(uploaded)}})
                raise AssertionError('changed upload was accepted')
            except AssertionError as error: assert 'has changed' in str(error),error
            uploaded.write_bytes(png)
            # A recorded upload can belong to a staged executor before the next
            # turn applies it. Replacing that generation must still reject reads.
            staged_target = dict(target, id=target['id'] + '-staged')
            with sqlite3.connect(directory/'state/state.sqlite') as db:
                db.execute('UPDATE uploaded_images SET target=? WHERE session_id=? AND path=?', (json.dumps(staged_target), created['id'], str(uploaded)))
                db.execute('INSERT INTO staged_session_targets VALUES(?,?,?)', (created['id'], '[]', json.dumps([staged_target])))
            assert call(host,token,{'ReadUploadedImage':{'id':created['id'],'path':str(uploaded)}})['dataBase64'] == base64.b64encode(png).decode()
            with sqlite3.connect(directory/'state/state.sqlite') as db:
                db.execute('DELETE FROM staged_session_targets WHERE session_id=?', (created['id'],))
            try:
                call(host,token,{'ReadUploadedImage':{'id':created['id'],'path':str(uploaded)}})
                raise AssertionError('detached image executor was accepted')
            except AssertionError as error: assert 'unavailable or replaced' in str(error),error
            with sqlite3.connect(directory/'state/state.sqlite') as db:
                db.execute('UPDATE uploaded_images SET target=? WHERE session_id=? AND path=?', (json.dumps(target), created['id'], str(uploaded)))
            codex.message('many-images', '\n'.join(f'![Image {i}](<{uploaded}>)' for i in range(24)))
            expect(page.locator('[data-item-id="many-images"] .image-thumbnail')).to_have_count(24, timeout=30000)
            try:
                call(host,token,{'ReadUploadedImage':{'id':created['id'],'path':str(report)}})
                raise AssertionError('arbitrary image path was read')
            except AssertionError as error: assert 'No recorded' in str(error),error
            article.get_by_role('button',name='Copy raw',exact=True).click()
            expect(article.get_by_role('button',name='Copied raw',exact=True)).to_be_visible()
            assert page.evaluate('navigator.clipboard.readText()')==source
            article.get_by_label('Format Markdown',exact=True).uncheck()
            expect(article.locator('math,table,iframe')).to_have_count(0)
            assert article.locator('.rich-message > pre').text_content()==source
            article.get_by_label('Format Markdown',exact=True).check()
            expect(article.locator('table')).to_have_count(1)
            expect_diagram(article)
            codex.message('alignment',r'''| Left | Center | Right | Default |
| :--- | :---: | ---: | --- |
| **Alpha** | middle | 12.50 | plain |
| Beta | $x^2$ | 3 | last |

| Other | Number |
| ---: | :--- |
| 7 | text |
''')
            aligned=page.locator('[data-item-id="alignment"]')
            expect(aligned.locator('table')).to_have_count(2)
            first_table=aligned.locator('table').first
            for row in first_table.locator('tr').all():
                assert row.locator('th,td').evaluate_all('(cells)=>cells.map(e=>getComputedStyle(e).textAlign)') == ['left','center','right','left']
            expect(first_table.locator('tbody > tr')).to_have_count(2)
            assert first_table.locator('tbody > tr').nth(1).evaluate('(e)=>getComputedStyle(e).backgroundColor') != 'rgba(0, 0, 0, 0)'
            for row in aligned.locator('table').nth(1).locator('tr').all():
                assert row.locator('th,td').evaluate_all('(cells)=>cells.map(e=>getComputedStyle(e).textAlign)') == ['right','left']
            codex.message('math-cases',r'''Inline \(a_1 + \sqrt{x}\).

$$
A=\begin{pmatrix}1 & 2 \\ 3 & 4\end{pmatrix},\qquad
|x|=\begin{cases}x & x\geq0 \\ -x & x<0\end{cases}
$$

Literal `\(x\)` and `$y$`.

```tex
\[not math\]
```
''')
            math_cases=page.locator('[data-item-id="math-cases"]')
            expect(math_cases.locator('math')).to_have_count(2)
            expect(math_cases.locator('mtable')).to_have_count(2)
            expect(math_cases.locator('.math-fallback')).to_have_count(0)
            expect(math_cases.locator('pre.code-block')).to_have_text('\\[not math\\]\n')
            codex.message('unfinished-math',r'Unfinished \(formula with a literal `\)` closer.')
            unfinished=page.locator('[data-item-id="unfinished-math"]')
            expect(unfinished.locator('math')).to_have_count(0)
            expect(unfinished.locator('code')).to_have_text(r'\)')
            codex.message('sequence','''```mermaid
sequenceDiagram
    actor User
    participant Browser
    participant Daemon
    User->>Browser: Open session
    Browser->>Daemon: Request snapshot
    alt Session exists
        Daemon-->>Browser: Session and messages
    else Session unavailable
        Daemon-->>Browser: Error details
    end
```''')
            sequence=page.locator('[data-item-id="sequence"]')
            expect(sequence.locator('iframe')).to_have_attribute('data-render-state','complete',timeout=30000)
            expect(sequence.frame_locator('iframe').locator('svg')).to_contain_text('Error details')
            # Web widgets show their target before installing any active link.
            article.locator('.markdown-link').filter(has_text='Documentation').click()
            web_popup=page.get_by_role('dialog',name='Documentation',exact=True)
            expect(web_popup.locator('a')).to_have_attribute('href','https://example.org/docs')
            expect(web_popup.locator('a strong')).to_have_text('example.org')
            assert not external,external
            page.keyboard.press('Escape')
            expect(web_popup).to_have_count(0)
            article.locator('.markdown-link').filter(has_text='Email').click()
            mail_popup=page.get_by_role('dialog',name='Email',exact=True)
            expect(mail_popup.locator('a')).to_have_count(0)
            page.mouse.click(1,1)
            expect(mail_popup).to_have_count(0)
            # Metadata is captured at completion using a real disposable executor.
            file_source=f'[Deployment status]({report}) and [Again]({report}) [Missing]({directory / "missing"}:1)'
            codex.message('file-links',file_source)
            file_article=page.locator('[data-item-id="file-links"]')
            expect(file_article.locator('.message-destination')).to_have_count(2)
            for _ in range(100):
                captured=call(host,token,{'MessageFiles':{'id':created['id'],'item':'file-links'}})
                if captured['files'] and all(c['state']!='pending' for f in captured['files'] for c in f['checks']):break
                time.sleep(.1)
            assert captured['files'][0]['checks'][0]['state']=='found',captured
            assert captured['files'][1]['checks'][0]['state']=='not-found',captured
            before=captured['files'][0]['checks'][0]['checked_at_ms']
            captured_executor=captured['files'][0]['checks'][0]['executor']
            file_article.locator('.markdown-link').filter(has_text='Deployment status').click()
            file_popup=page.get_by_role('dialog',name='Deployment status',exact=True)
            expect(file_popup.locator('.file-check-time')).to_contain_text('Checked')
            expect(file_popup.get_by_role('button',name='Show file',exact=True)).to_be_enabled()
            file_popup.get_by_role('button',name='Show file',exact=True).click()
            preview=page.get_by_role('dialog',name='File preview',exact=True)
            expect(preview.locator('.file-preview')).to_have_text(report.read_text())
            preview.get_by_role('button',name='Pretty JSON',exact=True).click()
            expect(preview.locator('.file-preview')).to_contain_text('"status": "ready"')
            report.write_text('{"status":"changed"}\n')
            with page.expect_download() as pending_download:
                preview.get_by_role('button',name='Download these bytes',exact=True).click()
            saved=pending_download.value
            assert Path(saved.path()).read_text()=='{"status":"ready","count":2}\n'
            preview.get_by_role('button',name='Close',exact=True).click()
            with page.expect_download() as pending_download:
                file_popup.get_by_role('button',name='Download file',exact=True).click()
            assert Path(pending_download.value.path()).read_text()==report.read_text()
            file_popup.get_by_role('button',name='Close',exact=True).click()
            # Duplicate completions do not update the recorded metadata timestamp.
            codex.message('file-links',file_source)
            captured=call(host,token,{'MessageFiles':{'id':created['id'],'item':'file-links'}})
            assert captured['files'][0]['checks'][0]['checked_at_ms']==before
            # Simulate a snapshot made before location suffix parsing was supported.
            with sqlite3.connect(directory/'state/state.sqlite') as db:
                saved_files=json.loads(db.execute('SELECT files FROM message_files WHERE session_id=? AND item_id=?',(created['id'],'file-links')).fetchone()[0])
                saved_files[1]['checks'][0]['path']=str(directory/'missing')+':1'
                for saved_file in saved_files:
                    for check in saved_file['checks']:check.pop('host',None)
                db.execute('DELETE FROM file_executor_hosts WHERE executor=?',(captured_executor,))
                db.execute('UPDATE message_files SET files=? WHERE session_id=? AND item_id=?',(json.dumps(saved_files),created['id'],'file-links'))
            # Explicit refresh discovers a formerly missing file, without rechecking neighbors.
            (directory/'missing').write_text('Now present\n')
            file_article.locator('.markdown-link').filter(has_text='Missing').click()
            missing_popup=page.get_by_role('dialog',name='Missing',exact=True)
            expect(missing_popup.get_by_role('button',name='Refresh',exact=True)).to_be_enabled()
            missing_popup.get_by_role('button',name='Refresh',exact=True).click()
            expect(missing_popup.get_by_role('button',name='Show file',exact=True)).to_be_enabled()
            refreshed=call(host,token,{'MessageFiles':{'id':created['id'],'item':'file-links'}})
            assert refreshed['files'][0]['checks'][0]['checked_at_ms']==before
            assert refreshed['files'][1]['checks'][0]['state']=='found'
            assert refreshed['files'][1]['checks'][0]['path']==str(directory/'missing')
            # Legacy links can be explicitly paired once with a durable registry target.
            missing_popup.get_by_role('button',name='Choose file host…',exact=True).click()
            host_popup=page.get_by_role('dialog',name='Choose file host',exact=True)
            expect(host_popup).to_be_visible()
            file_target=next(t for t in call(host,token,'Targets') if t['kind']=='external')
            host_popup.get_by_label('File host',exact=True).select_option(file_target['id'])
            host_popup.get_by_role('button',name='Use this host',exact=True).click()
            expect(host_popup).to_have_count(0)
            expect(missing_popup).to_contain_text('Saved host identity; session connection is not required.')
            missing_popup.get_by_role('button',name='Close',exact=True).click()
            # A receipted refresh cannot be replayed after the file changes again.
            import uuid
            refresh_id=str(uuid.uuid4())
            refresh_op={'RefreshMessageFile':{'id':created['id'],'item':'file-links','destination':str(directory/'missing')+':1'}}
            first_refresh=call(host,token,refresh_op,refresh_id)
            (directory/'missing').unlink()
            assert call(host,token,refresh_op,refresh_id)==first_refresh
            missing_again=call(host,token,refresh_op,str(uuid.uuid4()))
            assert missing_again['files'][1]['checks'][0]['state']=='not-found'
            assert missing_again['files'][1]['checks'][0]['metadata'] is None
            large=directory/'large.bin'
            with large.open('wb') as large_file:large_file.truncate(4*1024*1024+1)
            codex.message('large-file',f'[Large file]({large})')
            for _ in range(100):
                large_check=call(host,token,{'MessageFiles':{'id':created['id'],'item':'large-file'}})
                if large_check['files'] and large_check['files'][0]['checks'][0]['state']=='found':break
                time.sleep(.1)
            try:
                call(host,token,{'ReadMessageFile':{'id':created['id'],'item':'large-file','destination':str(large),'executor':captured_executor}})
            except AssertionError as error:assert '4 MiB' in str(error),error
            else:raise AssertionError('Oversized file unexpectedly read')
            codex.message('file-limit',' '.join(f'[Report {n}]({directory / f"report-{n}"})' for n in range(11)))
            for _ in range(100):
                limited=call(host,token,{'MessageFiles':{'id':created['id'],'item':'file-limit'}})
                if limited['files']:break
                time.sleep(.1)
            assert limited['files'][10]['checks']==[] and 'limit' in limited['files'][10]['note']
            svg_source='<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 240 100"><rect x="5" y="5" width="230" height="90" fill="#ff8800"/><text x="20" y="55" fill="black" font-size="18">Local drawing</text></svg>'
            codex.message('svg-preview','```svg\n'+svg_source+'\n```')
            svg_article=page.locator('[data-item-id="svg-preview"]')
            expect(svg_article.locator('.svg-thumbnail img')).to_be_visible()
            expect(svg_article.locator('svg')).to_have_count(0)
            expect_svg_image(svg_article.locator('.svg-thumbnail img'))
            svg_article.get_by_role('button',name='Enlarge SVG preview').click()
            svg_popup=page.get_by_role('dialog',name='SVG preview',exact=True)
            expect(svg_popup.locator('.svg-expanded img')).to_be_visible()
            expect_svg_image(svg_popup.locator('.svg-expanded img'))
            expect(svg_popup.locator('svg,object,iframe')).to_have_count(0)
            page.keyboard.press('Escape')
            expect(svg_popup).to_have_count(0)
            svg_article.get_by_text('SVG source',exact=True).click()
            assert svg_article.locator('.svg-source code').text_content()==svg_source+'\n'
            # Repeated IDs are isolated in separate image documents, including modal.
            rover = (ROOT / 'tests/fixtures/mars-rover.svg').read_text()
            for ident in ['svg-rover-one', 'svg-rover-two']:
                codex.message(ident, '```svg\n' + rover + '\n```')
                svg_case = page.locator(f'[data-item-id="{ident}"]')
                expect_svg_image(svg_case.locator('.svg-thumbnail img'))
            first_rover = page.locator('[data-item-id="svg-rover-one"]')
            first_rover.get_by_role('button', name='Enlarge SVG preview').click()
            expect_svg_image(page.get_by_role('dialog', name='SVG preview', exact=True).locator('.svg-expanded img'))
            assert page.locator('svg #sky, svg #solar, svg #title').count() == 0
            page.keyboard.press('Escape')
            expect(first_rover.locator('.svg-thumbnail img')).to_have_attribute('alt', 'A small rover, a big universe An orange exploration rover crosses a red Martian landscape beneath a pale sun and scattered stars.')
            # Canvas sampling confirms paint-server rendering, not just XML acceptance.
            pixels = first_rover.locator('.svg-thumbnail img').evaluate("""img => {
                const canvas = document.createElement('canvas');
                canvas.width = 800; canvas.height = 600;
                const ctx = canvas.getContext('2d'); ctx.drawImage(img, 0, 0, 800, 600);
                return [[400, 20], [400, 200], [611, 136], [350, 345], [360, 345]].map(([x,y]) => Array.from(ctx.getImageData(x,y,1,1).data));
            }""")
            assert pixels[0] != pixels[1], pixels
            assert pixels[2][:3] == [246, 223, 189], pixels
            assert pixels[3][:3] == [38, 62, 83] and pixels[3] != pixels[4], pixels
            for ident, body in [
                ('svg-script', '<script>window.svgExecuted=true</script>'),
                ('svg-external', '<rect fill="url(https://example.org/paint.svg#g)"/>'),
                ('svg-resource', '<image href="https://example.org/pixel.png"/>'),
            ]:
                codex.message(ident, '```svg\n<svg viewBox="0 0 10 10">' + body + '</svg>\n```')
                svg_case = page.locator(f'[data-item-id="{ident}"]')
                expect(svg_case).to_contain_text('SVG preview unavailable')
                expect(svg_case.locator('img,svg,iframe,object,script')).to_have_count(0)
            assert page.evaluate('window.svgExecuted === undefined')
            animated_source = (ROOT / 'tests/fixtures/animated-circle.svg').read_text()
            codex.message('svg-animated', '```svg\n' + animated_source + '\n```')
            animated_case = page.locator('[data-item-id="svg-animated"]')
            expect_svg_motion(animated_case.locator('.svg-thumbnail img'))
            animated_case.get_by_role('button', name='Enlarge SVG preview').click()
            expect_svg_motion(page.get_by_role('dialog', name='SVG preview', exact=True).locator('.svg-expanded img'))
            page.keyboard.press('Escape')
            animated_case.get_by_text('SVG source', exact=True).click()
            assert animated_case.locator('.svg-source code').text_content() == animated_source + '\n'
            transform_source = '<svg viewBox="0 0 100 100"><rect x="30" y="10" width="40" height="20" fill="orange"><animateTransform attributeName="transform" type="rotate" from="0 50 50" to="360 50 50" dur="2s" repeatCount="indefinite"/></rect></svg>'
            codex.message('svg-transform', '```svg\n' + transform_source + '\n```')
            expect_svg_motion(page.locator('[data-item-id="svg-transform"] .svg-thumbnail img'))
            for animation_id, target, values in [
                ('svg-animate-href', 'href', 'https://example.org/pixel;https://example.org/other'),
                ('svg-animate-paint', 'fill', 'red;url(https://example.org/paint.svg#g)'),
                ('svg-animate-handler', 'onload', 'alert(1);alert(2)'),
            ]:
                codex.message(animation_id, f'```svg\n<svg viewBox="0 0 100 100"><circle r="10"><animate attributeName="{target}" values="{values}" dur="1s"/></circle></svg>\n```')
                rejected_animation = page.locator(f'[data-item-id="{animation_id}"]')
                expect(rejected_animation).to_contain_text('SVG preview unavailable')
                expect(rejected_animation.locator('img')).to_have_count(0)
            drawing=directory/'drawing.svg'
            drawing.write_text(animated_source)
            codex.message('svg-file',f'[Drawing]({drawing})')
            for _ in range(100):
                drawing_check=call(host,token,{'MessageFiles':{'id':created['id'],'item':'svg-file'}})
                if drawing_check['files'] and drawing_check['files'][0]['checks'][0]['state']=='found':break
                time.sleep(.1)
            page.locator('[data-item-id="svg-file"] .markdown-link').click()
            page.get_by_role('dialog',name='Drawing',exact=True).get_by_role('button',name='Show file',exact=True).click()
            drawing_preview=page.get_by_role('dialog',name='File preview',exact=True)
            expect_svg_motion(drawing_preview.locator('.svg-thumbnail img'))
            drawing_preview.get_by_role('button',name='Enlarge SVG preview',exact=True).click()
            expect(page.get_by_role('dialog',name='SVG preview',exact=True).locator('.svg-expanded img')).to_be_visible()
            page.keyboard.press('Escape')
            expect(drawing_preview).to_be_visible()
            drawing_preview.get_by_role('button',name='Close',exact=True).click()
            page.get_by_role('dialog',name='Drawing',exact=True).get_by_role('button',name='Close',exact=True).click()
            # Ordinary file links use actual bytes, even without an image suffix.
            for ident, filename, payload in [('raster-file', 'picture.bin', png),
                                               ('broken-raster', 'broken.png', b'\x89PNG\r\n\x1a\ninvalid')]:
                raster = directory / filename
                raster.write_bytes(payload)
                codex.message(ident, f'[Linked image]({raster})')
                for _ in range(100):
                    snapshot = call(host, token, {'MessageFiles': {'id': created['id'], 'item': ident}})
                    if snapshot['files'] and snapshot['files'][0]['checks'][0]['state'] == 'found': break
                    time.sleep(.1)
                page.locator(f'[data-item-id="{ident}"] .markdown-link').click()
                popup = page.get_by_role('dialog', name='Linked image', exact=True)
                expect(popup.locator('img')).to_have_count(0)
                popup.get_by_role('button', name='Show file', exact=True).click()
                preview = page.get_by_role('dialog', name='File preview', exact=True)
                expect(preview.locator('img')).to_have_attribute('src', 'data:image/png;base64,' + base64.b64encode(payload).decode())
                if ident == 'raster-file':
                    page.wait_for_function('()=>document.querySelector("dialog img.image-expanded")?.naturalWidth === 1')
                    expect(preview).not_to_contain_text('Binary file')
                    expect(preview).not_to_contain_text('Preview shortened')
                else:
                    expect(preview.get_by_role('alert')).to_contain_text('Image could not be decoded')
                with page.expect_download() as downloaded:
                    preview.get_by_role('button', name='Download these bytes', exact=True).click()
                assert Path(downloaded.value.path()).read_bytes() == payload
                preview.get_by_role('button', name='Close', exact=True).click()
                popup.get_by_role('button', name='Close', exact=True).click()
            codex.message('svg-unsupported','```svg\n<svg viewBox="0 0 10 10"><filter/></svg>\n```')
            expect(page.locator('[data-item-id="svg-unsupported"]')).to_contain_text('SVG preview unavailable')
            expect(page.locator('[data-item-id="svg-unsupported"] svg')).to_have_count(0)
            codex.message('plain','A plain message. No formatting.')
            expect(page.locator('[data-item-id="plain"] .message-format-controls')).to_have_count(0)
            codex.message('broken',r'Invalid math $\unknowncommand{x}$ and **streaming')
            expect(page.locator('[data-item-id="broken"] .math-fallback')).to_contain_text(r'\unknowncommand')
            codex.message('resource-diagram','```mermaid\ngraph LR\nA-->B\nclick A href "https://example.org/diagram-link"\n```')
            resource_diagram=page.locator('[data-item-id="resource-diagram"]')
            expect(resource_diagram.locator('.mermaid-diagram')).to_contain_text('preview unavailable')
            expect(resource_diagram.locator('iframe,a,img,[src],[href]')).to_have_count(0)
            codex.message('bad-diagram','```mermaid\nnot valid diagram\n```')
            bad = page.locator('[data-item-id="bad-diagram"]')
            expect(bad.locator('iframe')).to_have_attribute('data-render-state','error',timeout=30000)
            expect(bad.frame_locator('iframe').locator('#diagram')).to_contain_text('preview unavailable')
            # An unrelated message must not reset the user's display choice.
            page.locator('.transcript').evaluate('(e)=>{e.scrollTop=0}')
            page.wait_for_timeout(200)
            article.get_by_label('Format Markdown',exact=True).uncheck()
            expect(article.locator('table')).to_have_count(0)
            article.get_by_label('Format Markdown',exact=True).evaluate('(e)=>{e.fixtureMarker=123; e.closest("article").fixtureMarker=456; e.closest(".conversation").fixtureMarker=789;}')
            codex.message('later','Another plain message')
            expect(page.locator('[data-item-id="later"]')).to_be_visible()
            assert article.get_by_label('Format Markdown',exact=True).evaluate('(e)=>e.fixtureMarker')==123
            expect(page.locator('[data-item-id="broken"]').get_by_label('Format Markdown',exact=True)).to_be_checked()
            expect(page.locator('[data-item-id="broken"] .math-fallback')).to_be_visible()
            expect(article.get_by_label('Format Markdown',exact=True)).not_to_be_checked()
            article.get_by_label('Format Markdown',exact=True).check()
            page.set_viewport_size({'width':390,'height':844})
            assert page.evaluate('document.documentElement.scrollWidth<=innerWidth')
            # Let the viewport and transcript follow-scroll settle before a pointer click.
            svg_article.get_by_role('button',name='Enlarge SVG preview').scroll_into_view_if_needed()
            page.wait_for_timeout(300)
            svg_article.get_by_role('button',name='Enlarge SVG preview').click()
            page.wait_for_timeout(200)
            expect(page.get_by_role('dialog',name='SVG preview').locator('.svg-expanded img')).to_be_visible()
            assert page.evaluate('document.documentElement.scrollWidth<=innerWidth')
            page.mouse.click(1,1)
            expect(page.get_by_role('dialog',name='SVG preview')).to_have_count(0)
            for fixture in [aligned, math_cases, sequence]:
                fixture.scroll_into_view_if_needed()
                assert fixture.evaluate('(e)=>e.getBoundingClientRect().right<=innerWidth'), fixture.get_attribute('data-item-id')
            expect(sequence.frame_locator('iframe').locator('svg')).to_contain_text('Session unavailable')
            article.scroll_into_view_if_needed()
            expect_diagram(article)
            page.screenshot(path=str(ROOT/'target/rich-messages-mobile.png'))
            context.set_offline(True)
            article.get_by_label('Format Markdown',exact=True).uncheck()
            article.get_by_label('Format Markdown',exact=True).check()
            expect_diagram(article)
            context.set_offline(False)
            # Switching targets cannot retarget links from old messages.
            call(host,token,{'SelectTargets':{'id':created['id'],'input':{'targets':[]}}})
            call(host,token,{'Prompt':{'id':created['id'],'text':'Fixture turn applies empty targets; no inference'}})
            file_article.locator('.markdown-link').filter(has_text='Deployment status').click()
            detached=page.get_by_role('dialog',name='Deployment status',exact=True)
            expect(detached.get_by_role('button',name='Show file',exact=True)).to_be_disabled()
            expect(detached.get_by_role('button',name='Refresh',exact=True)).to_be_disabled()
            try:
                call(host,token,{'RefreshMessageFile':{'id':created['id'],'item':'file-links','destination':str(report)}})
            except AssertionError as error:assert 'original file host' in str(error),error
            else:raise AssertionError('Detached executor unexpectedly refreshed')
            expect(detached).to_contain_text('No durable host identity was recorded')
            expect(detached.locator('.file-check-time')).to_contain_text('Checked')
            try:
                call(host,token,{'ReadMessageFile':{'id':created['id'],'item':'file-links','destination':str(report),'executor':captured_executor}})
            except AssertionError as error:
                assert 'no longer attached' in str(error),error
            else:raise AssertionError('Detached executor unexpectedly read')
            detached.get_by_role('button',name='Close',exact=True).click()
            # The explicitly paired file remains usable after removing session targets.
            (directory/'missing').write_text('Independent of session attachment\n')
            file_article.locator('.markdown-link').filter(has_text='Missing').click()
            detached_paired=page.get_by_role('dialog',name='Missing',exact=True)
            expect(detached_paired.get_by_role('button',name='Refresh',exact=True)).to_be_enabled()
            detached_paired.get_by_role('button',name='Refresh',exact=True).click()
            expect(detached_paired.get_by_role('button',name='Show file',exact=True)).to_be_enabled()
            detached_paired.get_by_role('button',name='Show file',exact=True).click()
            paired_preview=page.get_by_role('dialog',name='File preview',exact=True)
            expect(paired_preview.locator('.file-preview')).to_have_text('Independent of session attachment\n')
            paired_preview.get_by_role('button',name='Close',exact=True).click()
            detached_paired.get_by_role('button',name='Close',exact=True).click()

            assert not external,external
            assert not errors,errors
            browser.close()
        print('PASS: Markdown/math/Mermaid/SVG, confirmed URL images, explicit web links, real-executor file snapshots/previews/downloads, limits, detached executors, exact raw source, mobile/offline rendering')
    finally:
        for process in reversed(processes):
            process.terminate()
            try:process.wait(timeout=5)
            except subprocess.TimeoutExpired:process.kill();process.wait()
        server.shutdown()
