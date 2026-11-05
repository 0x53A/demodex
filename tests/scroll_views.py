# /// script
# dependencies = ["playwright"]
# ///
"""Scroll policy browser checks using production CSS; no daemon or inference.

Chromium touch emulation checks axis locking and nested horizontal gestures.
This does not emulate Safari's rubber-band rendering; also verify on an iPad.
"""
import os
from pathlib import Path

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[1]
CSS = '\n'.join((ROOT / 'crates/web' / name).read_text() for name in
                ('style.css', 'rich-messages.css', 'svg.css'))
BOX = 'position:relative;inset:auto;display:block;width:240px;height:300px;min-height:0;max-height:300px;margin:0;padding:10px;flex:none'
# Ancestors preserve the production selectors, including specialized modal forms.
PANELS = [
    ('aside', '', '', 'aside', ''),
    ('main', '<div class="layout">', '</div>', 'main', ''),
    ('login', '', '', 'div', 'login'),
    ('transcript', '', '', 'div', 'transcript'),
    ('takeover', '', '', 'div', 'app-notice'),
    ('global-error', '', '', 'div', 'global-error'),
    ('modal', '', '', 'div', 'modal-body'),
    ('inbox', '', '', 'div', 'session-inbox'),
    ('command', '', '', 'pre', 'approval-command'),
    ('permissions', '<div class="approval-permissions">', '</div>', 'pre', ''),
    ('details', '<div class="approval-details">', '</div>', 'pre', ''),
    ('weekly', '', '', 'div', 'weekly-details'),
    ('chat-error', '<div class="chat-main">', '</div>', 'div', 'error'),
    ('picker', '', '', 'div', 'picker-results'),
    ('form', '<div class="modal-body"><div class="ui-form">', '</div></div>', 'div', 'form-fields'),
    ('instructions', '', '', 'pre', 'instruction-file'),
    ('share', '', '', 'pre', 'share-text'),
    ('svg', '<div class="svg-source">', '</div>', 'pre', ''),
    ('textarea', '', '', 'textarea', ''),
    ('prompt-source', '<div class="prompt-original">', '</div>', 'pre', ''),
    ('prompt-overlay', '<div class="prompt-edit-area">', '</div>', 'pre', ''),
    ('prompt-edit', '<div class="prompt-edit-area">', '</div>', 'textarea', ''),
]


def swipe(page, x, y, dx, dy):
    cdp = page.context.new_cdp_session(page)
    cdp.send('Input.dispatchTouchEvent', {'type': 'touchStart', 'touchPoints': [{'x': x, 'y': y}]})
    for step in range(1, 11):
        cdp.send('Input.dispatchTouchEvent', {'type': 'touchMove', 'touchPoints': [
            {'x': x + dx * step / 10, 'y': y + dy * step / 10}]})
        page.wait_for_timeout(16)
    cdp.send('Input.dispatchTouchEvent', {'type': 'touchEnd', 'touchPoints': []})
    page.wait_for_timeout(100)
    cdp.detach()


def document(page, body):
    page.set_content('<style>' + CSS + '</style>' + body)


def main():
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path=os.environ.get('CHROME', '/run/current-system/sw/bin/google-chrome'),
                                    headless=True, args=['--no-sandbox'])
        for width, height in [(390, 844), (768, 1024), (1024, 768)]:
            page = browser.new_page(viewport={'width': width, 'height': height}, has_touch=True)
            for name, before, after, tag, cls in PANELS:
                def fixture(content):
                    return f'{before}<{tag} id="panel" class="{cls}" aria-label="Codex takeover" style="{BOX}">{content}</{tag}>{after}'
                document(page, fixture('Short content'))
                panel = page.locator('#panel')
                assert panel.evaluate('(e)=>e.scrollHeight<=e.clientHeight'), (width, name, 'short content overflows')
                assert panel.evaluate('(e)=>getComputedStyle(e).overflowX') == 'hidden', name
                assert panel.evaluate('(e)=>getComputedStyle(e).touchAction') == 'pan-y pinch-zoom', name
                assert panel.evaluate('(e)=>getComputedStyle(e).overscrollBehaviorX') == 'none', name
                # Long text wraps; oversized children exercise accidental horizontal overflow too.
                content = '\n'.join('long-content-' * 20 for _ in range(80))
                if tag not in ('pre', 'textarea'):
                    content = '<div style="height:1800px;width:900px">Oversized content</div>'
                document(page, fixture(content))
                # Highlight overlay ignores pointers intentionally; its textarea owns gestures.
                if name == 'prompt-overlay':
                    continue
                bounds = panel.bounding_box()
                swipe(page, bounds['x'] + 180, bounds['y'] + 240, -80, -180)
                pos = panel.evaluate('(e)=>[e.scrollLeft,e.scrollTop]')
                assert pos[0] == 0 and pos[1] > 0, (width, name, pos)
            # Horizontal children must remain touch-scrollable inside a locked transcript.
            for name, content in [
                ('code', '<div class="markdown"><pre class="code-block" id="wide">' + 'code ' * 100 + '</pre></div>'),
                ('table', '<div class="markdown"><div class="table-scroll" id="wide"><table style="width:1200px"><tr><td>Table</td></tr></table></div></div>'),
                ('math', '<div class="math-display" id="wide"><div style="width:1200px">Math</div></div>'),
                ('diff', '<div class="conversation"><pre class="diff-view" id="wide">' + 'diff ' * 100 + '</pre></div>'),
                ('file', '<pre class="file-preview" id="wide">' + 'file ' * 100 + '</pre>'),
                ('mermaid', '<div class="mermaid-diagram"><details open><summary>Source</summary><pre id="wide">' + 'diagram ' * 100 + '</pre></details></div>'),
                ('prompt', '<div class="prompt-nowrap prompt-original"><pre id="wide" style="height:100px;min-height:0">' + 'prompt ' * 100 + '</pre></div>'),
                ('prompt-input', '<div class="prompt-nowrap prompt-edit-area" style="height:100px;min-height:0"><textarea id="wide" style="min-height:0">' + 'prompt ' * 100 + '</textarea></div>'),
                ('attachments', '<div class="composer"><div class="composer-images" id="wide"><div style="flex:0 0 1200px">Images</div></div></div>'),
            ]:
                document(page, f'<div class="transcript" id="panel" style="{BOX}">{content}<div style="height:1500px"></div></div>')
                wide = page.locator('#wide')
                assert wide.evaluate('(e)=>getComputedStyle(e).overscrollBehaviorX') == 'none', name
                box = wide.bounding_box()
                swipe(page, box['x'] + 180, box['y'] + min(20, box['height'] / 2), -140, 0)
                assert wide.evaluate('(e)=>e.scrollLeft') > 0, (width, name, 'horizontal gesture blocked')
                assert page.locator('#panel').evaluate('(e)=>e.scrollLeft') == 0, (width, name, 'parent drifted')
            print(f'{width}x{height}: vertical panels, short content and nested horizontal gestures passed')
            page.close()
        browser.close()


if __name__ == '__main__':
    main()
