# /// script
# dependencies = ["playwright"]
# ///
"""Large imported history over real Wormhole; disposable state, no Codex calls."""
import json
from pathlib import Path
import socket
import sqlite3
import subprocess
import tempfile
import time
import urllib.request
from playwright.sync_api import sync_playwright, expect

ROOT = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='demodex-large-history-') as temporary:
    data = Path(temporary)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    origin = f'http://127.0.0.1:{port}'
    with (data/'daemon.log').open('w') as log:
        daemon = subprocess.Popen([str(ROOT/'target/rust-pwa/debug/demodex'), '--bind', f'127.0.0.1:{port}', '--data-dir', str(data), '--web-dir', str(ROOT/'web/.rust-dist')], stdout=log, stderr=log)
        try:
            for _ in range(200):
                try:
                    with socket.create_connection(('127.0.0.1',port),timeout=.1): break
                except OSError: time.sleep(.05)
            else: raise AssertionError('daemon did not start')
            token = (data/'access-token').read_text().strip()
            request = urllib.request.Request(origin+'/api/sessions', data=json.dumps({'name':'Large imported history','endpoint':'ws://127.0.0.1:1','targets':[]}).encode(), headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'})
            with urllib.request.urlopen(request) as response: session = json.load(response)
            items = [{'id':'old-answer','type':'agentMessage','text':'Large history successfully restored'},
                     {'id':'old-tool','type':'commandExecution','command':'pwd','aggregatedOutput':'/workspace'}]
            for index in range(3000):
                items.append({'id':f'history-{index}','type':'agentMessage','text':f'Day {index//100+1}: historical message {index}'})
                if index % 100 == 99:
                    items.append({'id':f'compaction-{index}','type':'contextCompaction'})
            message = {'method':'demodex/threadSnapshot','params':{'thread':{'turns':[{'id':'old-turn','items':items}]},'unknownFutureMetadata':'x'*(16*1024*1024)}}
            with sqlite3.connect(data/'state.sqlite') as db:
                db.execute('INSERT INTO events(session_id,message) VALUES(?,?)',(session['id'],json.dumps(message)))
            with sync_playwright() as playwright:
                browser=playwright.chromium.launch(executable_path='/run/current-system/sw/bin/google-chrome',headless=True,args=['--no-sandbox'])
                context=browser.new_context(service_workers='block')
                context.add_init_script('sessionStorage.setItem('+json.dumps('demodex-token:'+origin)+','+json.dumps(token)+');')
                page=context.new_page()
                errors=[]
                page.on('pageerror',lambda error: errors.append(str(error)))
                page.goto(origin)
                expect(page.locator('header .indicator')).to_have_text('CONNECTED',timeout=20000)
                page.locator('button.session').filter(has_text='Large imported history').click()
                expect(page.get_by_text('Large history successfully restored',exact=True)).to_be_visible(timeout=30000)
                expect(page.get_by_role('alert')).to_have_count(0)
                expect(page.locator('.conversation article')).to_have_count(len(items))
                transcript = page.locator('.transcript')
                transcript.evaluate('e=>{e.scrollTop=0;e.dispatchEvent(new Event("scroll"));}')
                old_tool = page.locator('[data-item-id="old-tool"] details')
                old_tool.locator('summary').click()
                page.evaluate('''() => {
                    window.oldArticle=document.querySelector('[data-item-id="old-answer"]');
                    window.oldChanges=[];
                    window.oldObserver=new MutationObserver(records=>oldChanges.push(...records));
                    oldObserver.observe(oldArticle,{subtree:true,childList:true,characterData:true,attributes:true});
                    const range=document.createRange();
                    range.selectNodeContents(oldArticle.querySelector('pre'));
                    getSelection().removeAllRanges();getSelection().addRange(range);
                }''')
                before = transcript.evaluate('e=>e.scrollTop')
                def append(messages):
                    with sqlite3.connect(data/'state.sqlite') as db:
                        db.executemany('INSERT INTO events(session_id,message) VALUES(?,?)',
                            [(session['id'],json.dumps(message)) for message in messages])
                    page.evaluate("window.dispatchEvent(new Event('online'))")
                append([{'method':'item/agentMessage/delta','params':{'itemId':'live','delta':'stream '}} for _ in range(100)])
                live = page.locator('[data-item-id="live"] pre')
                expect(live).to_have_text('stream '*100,timeout=30000)
                assert transcript.evaluate('e=>Math.abs(e.scrollTop-'+str(before)+')<2')
                assert old_tool.evaluate('e=>e.open')
                assert page.evaluate("""oldArticle===document.querySelector('[data-item-id="old-answer"]') && oldChanges.length===0""")
                assert page.evaluate('getSelection().toString()')=='Large history successfully restored'
                # A resumed post-compaction snapshot omits earlier turns. Keep
                # their nodes and expanders; replace the completed streamed item.
                append([
                    {'method':'item/completed','params':{'item':{'id':'live','type':'agentMessage','text':'Final streamed answer'}}},
                    {'method':'demodex/threadSnapshot','params':{'thread':{'turns':[{'items':[
                        {'id':'live','type':'agentMessage','text':'Final streamed answer'},
                        {'id':'next-compaction','type':'contextCompaction'},
                        {'id':'next-day','type':'agentMessage','text':'Work after another compaction'}
                    ]}]}}}
                ])
                expect(page.locator('[data-item-id="next-day"]')).to_have_text('agentMessageWork after another compaction',timeout=30000)
                expect(live).to_have_text('Final streamed answer')
                expect(page.locator('.conversation article')).to_have_count(len(items)+3)
                assert page.evaluate('oldChanges.length===0')
                assert old_tool.evaluate('e=>e.open')
                assert transcript.evaluate('e=>Math.abs(e.scrollTop-'+str(before)+')<2')
                # Updating an earlier item replaces its content in place.
                append([{'method':'item/completed','params':{'item':{'id':'old-tool','type':'commandExecution','command':'pwd','aggregatedOutput':'/updated'}}}])
                expect(old_tool.locator('pre')).to_have_text('/updated',timeout=30000)
                assert old_tool.evaluate('e=>e.open')
                page.get_by_role('button',name='Jump to latest',exact=True).click()
                append([{'method':'item/agentMessage/delta','params':{'itemId':'next-day','delta':' — continued'}}])
                expect(page.locator('[data-item-id="next-day"]')).to_contain_text('continued',timeout=30000)
                page.wait_for_function('document.querySelector(".transcript").scrollHeight-document.querySelector(".transcript").scrollTop-document.querySelector(".transcript").clientHeight<=1')
                page.reload()
                expect(page.get_by_text('Large history successfully restored',exact=True)).to_be_visible(timeout=30000)
                assert not errors,errors
                browser.close()
            print('PASS: 16 MiB snapshot, 3,000 messages, 30 compactions, incremental streaming/snapshots, stable nodes/selection/expanders, follow and reload')
        finally:
            daemon.terminate()
            daemon.wait(timeout=15)
