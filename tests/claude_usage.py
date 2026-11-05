# /// script
# dependencies = ["playwright"]
# ///
"""Browser half of Rust claude_usage_browser fixture; no real credentials/API requests."""
import os
import sys
from playwright.sync_api import sync_playwright, expect

origin, scenario = sys.argv[1:]
with sync_playwright() as pw:
    browser = pw.chromium.launch(executable_path=os.environ.get('CHROME','/run/current-system/sw/bin/google-chrome'),headless=True,args=['--no-sandbox'])
    context = browser.new_context(viewport={'width':390,'height':844})
    context.add_init_script('window.fixtureSockets=[]; window.WebSocket=class extends WebSocket {constructor(...args){super(...args);window.fixtureSockets.push(this);}};')
    page = context.new_page()
    errors = []
    page.on('pageerror', lambda e: errors.append(str(e)))
    page.goto(origin)
    page.get_by_role('button',name='+ connection',exact=True).click()
    page.get_by_label('Host URL').fill(origin)
    page.get_by_label('Access token').fill('usage-browser-fixture')
    page.get_by_role('button',name='Save and connect',exact=True).click()
    expect(page.locator('header .indicator')).to_have_text('connected to')
    usage = page.get_by_label('Claude usage',exact=True)
    if scenario == 'signedout':
        expect(usage).to_have_count(0)
    else:
        expect(usage).to_be_visible()
        if scenario == 'success':
            expect(usage.locator('summary meter')).to_have_count(2)
            expect(usage.locator('summary').get_by_label('Claude 5h: 87.5% remaining')).to_have_attribute('value','87.5')
        if scenario == 'weekly_only':
            expect(usage.locator('summary .quota-row')).to_have_count(1)
            expect(usage.locator('summary .quota-period')).to_have_text('weekly:')
        usage.locator('summary').click()
        if scenario == 'success':
            expect(usage.get_by_label('Remaining Claude 5h capacity')).to_have_attribute('value','87.5')
            expect(usage.get_by_label('Remaining Claude Weekly capacity')).to_have_attribute('value','0')
            expect(usage).to_contain_text('resets in')
            expect(usage).to_contain_text('Last updated:')
        elif scenario == 'partial':
            expect(usage.locator('summary').get_by_label('Claude weekly: unavailable')).to_be_visible()
            expect(usage.locator('.weekly-details meter')).to_have_count(1)
            expect(usage).to_contain_text('reset time unavailable')
        elif scenario == 'error':
            expect(usage.locator('meter')).to_have_count(0)
            expect(usage).to_contain_text('Claude usage request timed out')
        for width in (320,390,1200):
            page.set_viewport_size({'width':width,'height':844})
            assert page.evaluate('document.documentElement.scrollWidth <= innerWidth'), width
            box=usage.locator('.weekly-details').bounding_box()
            assert box and box['x'] >= -1 and box['x']+box['width'] <= width+1, box
        if scenario == 'success':
            page.set_viewport_size({'width':390,'height':844})
            usage.locator('summary').click()
            page.screenshot(path=os.environ.get('CLAUDE_USAGE_SCREENSHOT','/tmp/demodex-claude-usage.png'))
        # Reconnection must not present old quota as current while offline.
        context.set_offline(True)
        page.evaluate('window.fixtureSockets.forEach(s=>s.close())')
        expect(usage.locator('summary')).to_have_text('Claude:disconnected')
        expect(usage.locator('meter')).to_have_count(0)
    assert not errors, errors
    browser.close()
    print('PASS: Claude usage browser',scenario)
