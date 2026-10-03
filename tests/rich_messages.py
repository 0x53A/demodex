# /// script
# dependencies = ["playwright", "websockets>=15"]
# ///
"""Markdown, native MathML and sandboxed local Mermaid; no inference."""
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
            file_source=f'[Deployment status]({report}) and [Again]({report}) [Missing]({directory / "missing"})'
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
            expect(svg_article.locator('.svg-thumbnail svg')).to_be_visible()
            assert svg_article.locator('svg').evaluate('(e)=>e.namespaceURI')=='http://www.w3.org/2000/svg'
            expect(svg_article.locator('svg text')).to_have_text('Local drawing')
            svg_article.get_by_role('button',name='Enlarge SVG preview').click()
            svg_popup=page.get_by_role('dialog',name='SVG preview',exact=True)
            expect(svg_popup.locator('svg')).to_be_visible()
            expect(svg_popup.locator('a,image,foreignObject,[href],[src]')).to_have_count(0)
            page.keyboard.press('Escape')
            expect(svg_popup).to_have_count(0)
            svg_article.get_by_text('SVG source',exact=True).click()
            assert svg_article.locator('.svg-source code').text_content()==svg_source+'\n'
            drawing=directory/'drawing.svg'
            drawing.write_text(svg_source)
            codex.message('svg-file',f'[Drawing]({drawing})')
            for _ in range(100):
                drawing_check=call(host,token,{'MessageFiles':{'id':created['id'],'item':'svg-file'}})
                if drawing_check['files'] and drawing_check['files'][0]['checks'][0]['state']=='found':break
                time.sleep(.1)
            page.locator('[data-item-id="svg-file"] .markdown-link').click()
            page.get_by_role('dialog',name='Drawing',exact=True).get_by_role('button',name='Show file',exact=True).click()
            drawing_preview=page.get_by_role('dialog',name='File preview',exact=True)
            expect(drawing_preview.locator('svg')).to_be_visible()
            drawing_preview.get_by_role('button',name='Enlarge SVG preview',exact=True).click()
            expect(page.get_by_role('dialog',name='SVG preview',exact=True).locator('svg')).to_be_visible()
            page.keyboard.press('Escape')
            expect(drawing_preview).to_be_visible()
            drawing_preview.get_by_role('button',name='Close',exact=True).click()
            page.get_by_role('dialog',name='Drawing',exact=True).get_by_role('button',name='Close',exact=True).click()
            codex.message('svg-unsupported','```svg\n<svg viewBox="0 0 10 10"><defs/></svg>\n```')
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
            expect(page.get_by_role('dialog',name='SVG preview').locator('svg')).to_be_visible()
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
            expect(detached).to_contain_text('Original executor is no longer attached')
            expect(detached.locator('.file-check-time')).to_contain_text('Checked')
            try:
                call(host,token,{'ReadMessageFile':{'id':created['id'],'item':'file-links','destination':str(report),'executor':captured_executor}})
            except AssertionError as error:
                assert 'no longer attached' in str(error),error
            else:raise AssertionError('Detached executor unexpectedly read')
            detached.get_by_role('button',name='Close',exact=True).click()
            assert not external,external
            assert not errors,errors
            browser.close()
        print('PASS: Markdown/math/Mermaid/SVG, explicit web links, real-executor file snapshots/previews/downloads, limits, detached executors, exact raw source, mobile/offline rendering')
    finally:
        for process in reversed(processes):
            process.terminate()
            try:process.wait(timeout=5)
            except subprocess.TimeoutExpired:process.kill();process.wait()
        server.shutdown()
