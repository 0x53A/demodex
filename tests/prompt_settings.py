# /// script
# dependencies = ["playwright", "websockets>=15"]
# ///
"""Prompt layers, defaults, instruction files and editable diff; no model inference."""
import copy
import json
import os
import re
from pathlib import Path
import socket
import signal
import subprocess
import tempfile
import time
from playwright.sync_api import sync_playwright, expect
from wormhole_client import call
from websockets.sync.client import unix_connect

ROOT=Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='demodex-prompt-settings-') as tmp:
    root=Path(tmp); workspace=root/'workspace';workspace.mkdir();(workspace/'.git').mkdir()
    profile=root/'profile';profile.mkdir()
    (profile/'base.txt').write_text('Configured profile base')
    original=f'model_instructions_file = "{profile}/base.txt"\ndeveloper_instructions = "Operator layer"\nproject_doc_fallback_filenames = ["CLAUDE.md"]\n'
    (profile/'config.toml').write_text(original)
    (profile/'AGENTS.md').write_text('Global instruction fixture')
    catalog=subprocess.check_output(['codex','debug','models','--bundled'],env={**os.environ,'CODEX_HOME':str(profile)})
    (profile/'models_cache.json').write_bytes(catalog)
    configured_model=json.loads(catalog)['models'][0]['slug']
    original+=f'model = "{configured_model}"\nmodel_reasoning_effort = "high"\n'
    (profile/'config.toml').write_text(original)
    (workspace/'CLAUDE.md').write_text('Project fallback fixture')
    with socket.socket() as sock:
        sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
    data=root/'state';binary=os.environ.get('DEMODEX_BIN',str(ROOT/'target/rust-pwa/debug/demodex'))
    with (root/'daemon.log').open('w+') as log:
        child=subprocess.Popen([binary,'--data-dir',str(data),'--bind',f'127.0.0.1:{port}','--host-workspace',str(workspace),'--codex-home',str(profile),'--web-dir',os.environ.get('DEMODEX_WEB',str(ROOT/'web/.rust-dist'))],stdout=log,stderr=log,start_new_session=True)
        try:
            for _ in range(200):
                assert child.poll() is None
                try:
                    with socket.create_connection(('127.0.0.1',port),timeout=.1):break
                except OSError:time.sleep(.05)
            token=(data/'access-token').read_text().strip()
            def api(op):return call(f'http://127.0.0.1:{port}',token,op)
            baseline=api('PromptSettings');model=baseline['default_model']
            assert baseline['defaults'][model]['effective_text']=='Configured profile base'
            assert baseline['defaults'][model]['text']!='Configured profile base'
            settings=baseline['settings']
            settings['models'][model]={'text':'Replacement <script>literal</script>','reviewed_default':baseline['defaults'][model]['fingerprint']}
            settings['append']='Shared appendix'
            settings['integration']={'text':'Custom integration','reviewed_default':baseline['integration_default']['fingerprint']}
            saved=api({'SavePromptSettings':{'expected_revision':0,'settings':settings}})
            assert saved['revision']==1
            try:
                api({'SavePromptSettings':{'expected_revision':0,'settings':settings}})
                raise RuntimeError('stale save accepted')
            except AssertionError as error:assert 'another window' in str(error)
            session=api({'CreateSession':{'input':{'name':'Prompt layers fixture','targets':[{'id':'host','cwd':str(workspace)}],'sandbox':'read-only','include_project':False}}})
            assert session['status']=='connected',session
            applied=api({'SessionPromptSettings':{'id':session['id']}})
            assert applied['applied']['base']=='Replacement <script>literal</script>\n\nShared appendix',applied
            assert applied['applied']['developer']=='Operator layer\n\nCustom integration'
            assert applied['applied']['include_project'] is False
            # A fresh thread has no rollout yet. Effort/tier/no-op changes must
            # update settings in place instead of attempting an impossible resume.
            current=api({'Detail':{'id':session['id']}})['controls']['settings']['effective']
            assert current['model']==configured_model and current['effort']=='high',current
            choice=next(m for m in api({'Models':{'id':session['id']}})['data'] if m['model']==current['model'])
            effort=next(e['reasoningEffort'] for e in choice['supportedReasoningEfforts'] if e['reasoningEffort']!=current['effort'])
            change={'model':current['model'],'effort':effort,'serviceTier':current['serviceTier']}
            api({'Model':{'id':session['id'],'input':change}})
            api({'Model':{'id':session['id'],'input':change}})
            tiers=[t['id'] for t in choice.get('serviceTiers',[]) if t['id']!='default' and t['id']!=current['serviceTier']]
            if tiers:
                api({'Model':{'id':session['id'],'input':dict(change,serviceTier=tiers[0])}})
            detail=api({'Detail':{'id':session['id']}})
            assert detail['session']['status']=='connected',detail
            assert detail['session']['thread_id']==session['thread_id']
            assert detail['controls']['settings']['effective']['effort']==effort
            assert api({'SessionPromptSettings':{'id':session['id']}})['applied']==applied['applied']
            files=api({'InstructionFiles':{'target':{'id':'host','cwd':str(workspace)}}})['files']
            assert {f['text'] for f in files}=={'Global instruction fixture','Project fallback fixture'},files
            # Removing a replacement restores the current profile prompt before appending.
            settings['models'].clear()
            api({'SavePromptSettings':{'expected_revision':1,'settings':settings}})
            assert api({'SessionPromptSettings':{'id':session['id']}})['pending']
            # Persist disposable history without a turn/start or model call.
            with unix_connect(str(data/'runtime/ipc/app.sock')) as ws:
                def rpc(method,params,ident):
                    ws.send(json.dumps({'id':ident,'method':method,'params':params}))
                    while True:
                        result=json.loads(ws.recv(timeout=30))
                        if result.get('id')==ident:
                            assert 'error' not in result,result
                            return result['result']
                rpc('initialize',{'clientInfo':{'name':'prompt-fixture','version':'1'},'capabilities':{'experimentalApi':True}},1)
                ws.send(json.dumps({'method':'initialized','params':{}}))
                rpc('thread/inject_items',{'threadId':session['thread_id'],'items':[{'type':'message','role':'user','content':[{'type':'input_text','text':'Persisted fixture history; no model turn.'}]}]},2)
            updated=api({'ApplySessionPromptSettings':{'id':session['id'],'include_project':True}})
            assert updated['applied']['base']=='Configured profile base\n\nShared appendix',updated
            assert updated['applied']['include_project'] is True
            fresh=api({'CreateSession':{'input':{'name':'Default base fixture','targets':[],'sandbox':'read-only','include_project':True}}})
            assert fresh['status']=='connected',fresh
            assert api({'SessionPromptSettings':{'id':fresh['id']}})['applied']['base']=='Configured profile base\n\nShared appendix'
            # Model changes use that model's replacement and preserve the shared suffix.
            models=api({'Models':{'id':session['id']}})['data']
            other=next(m for m in models if m['model']!=model and m.get('supportedReasoningEfforts') and baseline['defaults'].get(m['model'],{}).get('text'))
            settings['models'][other['model']]={'text':'Other model replacement','reviewed_default':baseline['defaults'][other['model']]['fingerprint']}
            api({'SavePromptSettings':{'expected_revision':2,'settings':settings}})
            api({'Model':{'id':session['id'],'input':{'model':other['model'],'effort':other['defaultReasoningEffort'],'serviceTier':None}}})
            switched=api({'SessionPromptSettings':{'id':session['id']}})
            assert switched['applied']['model']==other['model'],switched
            assert switched['applied']['base']=='Other model replacement\n\nShared appendix',switched
            with sync_playwright() as pw:
                browser=pw.chromium.launch(executable_path=os.environ.get('CHROME','/run/current-system/sw/bin/google-chrome'),headless=True,args=['--no-sandbox'])
                page=browser.new_page(viewport={'width':1200,'height':900})
                page.goto(f'http://127.0.0.1:{port}')
                page.get_by_role('button',name='+ connection',exact=True).click()
                page.get_by_label('Access token').fill(token)
                page.get_by_role('button',name='Save and connect',exact=True).click()
                expect(page.locator('header .indicator')).to_have_text('connected to',timeout=20000)
                # Folder creation inherits its stable host and exact project path.
                host_group=page.locator('.target-folder[data-target-id="host"]')
                host_group.get_by_role('button',name='+ Session',exact=True).first.click()
                creation=page.get_by_role('dialog',name='New Session',exact=True)
                expect(creation.locator('.creation-target input[type="checkbox"]').first).to_be_checked()
                expect(creation.locator('.target-directory input').first).to_have_value(str(workspace))
                runtime_models=api('RuntimeModels')
                assert runtime_models['defaults']['model']==configured_model
                assert runtime_models['defaults']['effort']=='high'
                default_model=next(m for m in runtime_models['data'] if m['model']==configured_model)
                expect(creation.get_by_label('Model',exact=True).locator('option:checked')).to_have_text(f"Default ({default_model['displayName']})")
                expect(creation.get_by_label('Reasoning effort',exact=True).locator('option:checked')).to_have_text('Default (high)')
                tier=runtime_models['defaults']['serviceTier']
                tier_label=next((t['name'] for t in default_model.get('serviceTiers',[]) if t['id']==tier),tier or 'Standard')
                expect(creation.get_by_label('Service tier',exact=True).locator('option:checked')).to_have_text(f'Default ({tier_label})')
                creation.get_by_label('Model',exact=True).select_option(model)
                efforts=creation.get_by_label('Reasoning effort',exact=True).locator('option')
                for index in range(efforts.count()):
                    option=efforts.nth(index)
                    if option.get_attribute('value'):
                        assert option.inner_text()==option.get_attribute('value')
                creation.get_by_role('button',name='Read instruction files',exact=True).click()
                preview=page.get_by_role('dialog',name='Instruction files (read-only)',exact=True)
                expect(preview.locator('details')).to_have_count(2)
                expect(preview.locator('[role="alert"]')).to_have_count(0)
                preview.get_by_role('button',name='Close',exact=True).click()
                creation.get_by_role('button',name='Close',exact=True).click()
                page.get_by_role('button',name='Server settings',exact=True).click()
                settings_dialog=page.get_by_role('dialog',name='Server settings',exact=True)
                expect(settings_dialog.get_by_label('Append instructions for all models',exact=True)).to_have_value('Shared appendix',timeout=20000)
                settings_dialog.get_by_role('button',name='+ Add model override',exact=True).click()
                selectors=settings_dialog.get_by_label('Prompt model',exact=True)
                selector=next(selectors.nth(i) for i in range(selectors.count()) if selectors.nth(i).input_value()!=other['model'])
                selector.select_option(model)
                settings_dialog.get_by_role('button',name=f'Edit prompt for {model}',exact=True).click()
                editor=page.get_by_role('dialog',name=f'System prompt · {model}',exact=True)
                original_text=editor.get_by_label('Prompt text',exact=True).input_value()
                # Numbers follow source lines, including empty lines, while long
                # prose and unbroken words wrap without changing the draft.
                draft=('Wrapped prose ' * 35)+'\n\n'+('x' * 300)+'\n'+('\tIndented line\n' * 45)
                prompt=editor.get_by_label('Prompt text',exact=True)
                prompt.fill(draft)
                expect(editor.locator('.prompt-edit-area .prompt-line-number')).to_have_count(len(draft.split('\n')))
                for width in (1200,390):
                    page.set_viewport_size({'width':width,'height':900})
                    for mode in ('Diff','Edit'):
                        editor.get_by_role('button',name=mode,exact=True).click()
                        expect(prompt).to_have_value(draft)
                        assert prompt.evaluate('(t)=>t.scrollWidth<=t.clientWidth'), 'prompt scrolls horizontally'
                        assert editor.locator('.prompt-edit-area .prompt-line').first.evaluate('(l)=>l.clientHeight>21'), 'prose did not wrap'
                        assert editor.locator('.prompt-edit-area .prompt-line').nth(2).evaluate('(l)=>l.clientHeight>21'), 'unbroken word did not wrap'
                        assert editor.locator('.prompt-edit-area').evaluate('''(area)=>{
                            const t=area.querySelector('textarea'), p=area.querySelector('pre');
                            return Math.abs(t.scrollHeight-p.scrollHeight)<=2;
                        }'''), 'line numbers and text have different layout heights'
                        prompt.evaluate('(t)=>{t.scrollTop=230;t.dispatchEvent(new Event("scroll"));}')
                        expect(editor.locator('.prompt-edit-area pre')).to_have_js_property('scrollTop',230)
                page.set_viewport_size({'width':1200,'height':900})
                editor.get_by_role('button',name='Diff',exact=True).click()
                wrap=editor.get_by_label('Word wrapping',exact=True)
                expect(wrap).to_be_checked()
                wrap.uncheck()
                assert prompt.evaluate('(t)=>t.scrollWidth>t.clientWidth')
                expect(prompt).to_have_value(draft)
                wrap.check()
                # A small wording edit highlights only the replacement, and
                # preserves the common words visibly in both panes.
                default_text=baseline['defaults'][model]['text']
                first_word=re.search(r'\w+',default_text).group()
                prompt.fill(default_text.replace(first_word, 'Someone', 1))
                expect(editor.locator('.prompt-original .diff-word').first).to_have_text(first_word)
                expect(editor.locator('.prompt-replacement .diff-word').first).to_have_text('Someone')
                expect(editor.locator('.prompt-replacement .diff-context').first).to_be_visible()
                editor.get_by_label('Prompt text',exact=True).fill('Editable replacement\n<script>inert</script>')
                expect(editor.locator('.diff-add').first).to_be_visible()
                assert editor.locator('script').count()==0
                editor.get_by_role('button',name='Edit',exact=True).click()
                expect(editor.get_by_label('Prompt text',exact=True)).to_have_value('Editable replacement\n<script>inert</script>')
                editor.get_by_role('button',name='Save',exact=True).click()
                expect(editor).not_to_be_visible()
                expect(settings_dialog.get_by_role('button',name='Save prompt settings',exact=True)).to_be_disabled()
                # Make the baseline change in the disposable profile and detect it on refresh.
                cache=profile/'models_cache.json';catalog=json.loads(cache.read_text())
                entry=next(m for m in catalog['models'] if m['slug']==model)
                entry['model_messages']['instructions_template']=original_text+'\nChanged default fixture'
                cache.write_text(json.dumps(catalog))
                settings_dialog.get_by_role('button',name='Refresh Codex defaults',exact=True).click()
                expect(settings_dialog.get_by_text('Codex default changed — review diff',exact=True)).to_be_visible(timeout=10000)
                settings_dialog.get_by_role('button',name=f'Edit prompt for {model}',exact=True).click()
                editor.get_by_role('button',name='Diff',exact=True).click()
                expect(editor.locator('.prompt-original')).to_contain_text('Changed default fixture')
                page.set_viewport_size({'width':390,'height':844})
                assert editor.evaluate('(d)=>d.scrollWidth<=d.clientWidth+2')
                editor.get_by_role('button',name='Cancel',exact=True).click()
                browser.close()
            # A configured profile prompt remains effective without a catalogue
            # entry, including when the operator saves unchanged server defaults.
            snapshot=api('PromptSettings')
            defaults={'models':{},'append':'','integration':None,'include_project':True}
            api({'SavePromptSettings':{'expected_revision':snapshot['revision'],'settings':defaults}})
            cache_data=json.loads((profile/'models_cache.json').read_text())
            cache_data['models']=[entry for entry in cache_data['models'] if entry['slug']!=model]
            (profile/'models_cache.json').write_text(json.dumps(cache_data))
            missing=api('PromptSettings')['defaults'][model]
            assert missing['error'] and missing['effective_text']=='Configured profile base',missing
            inherited=api({'CreateSession':{'input':{'name':'Uncached configured prompt','targets':[],'sandbox':'read-only'}}})
            assert inherited['status']=='connected',inherited
            assert api({'SessionPromptSettings':{'id':inherited['id']}})['applied']['base']=='Configured profile base'
            assert (profile/'config.toml').read_text()==original
            assert (profile/'AGENTS.md').read_text()=='Global instruction fixture'
            assert (workspace/'CLAUDE.md').read_text()=='Project fallback fixture'
            print('PASS: real Codex prompt layers, stale saves, inclusion, previews, editable diff, changed defaults and mobile layout; no inference')
        except Exception:
            log.flush();log.seek(0);print(log.read()[-5000:]);raise
        finally:
            try: os.killpg(child.pid,signal.SIGTERM)
            except ProcessLookupError: pass
            child.wait(timeout=30)
