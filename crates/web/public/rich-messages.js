/* Diagram execution is isolated in an opaque-origin sandbox. No model HTML is
 * inserted into the application document. The vendored renderer works offline. */
(() => {
  const renderer = new URL('./vendor/mermaid-12.0.0.min.js', document.currentScript.src).href;
  const escape = s => s.replaceAll('&', '&amp;').replaceAll('"', '&quot;').replaceAll('<', '&lt;');
  let rendererBlob;
  const localRenderer = () => rendererBlob ||= fetch(renderer, {credentials:'omit'})
    .then(response => { if (!response.ok) throw new Error('Renderer unavailable'); return response.blob(); })
    .then(blob => new Blob([blob], {type:'text/javascript'}))
    .catch(error => { rendererBlob = undefined; throw error; });
  window.demodexRich = {
    copy: source => navigator.clipboard.writeText(source),
    download(encoded, name) {
      const bytes = Uint8Array.from(atob(encoded), c => c.charCodeAt(0));
      const url = URL.createObjectURL(new Blob([bytes], {type:'application/octet-stream'}));
      const anchor = document.createElement('a');
      anchor.href = url;
      anchor.download = name.replace(/[\\/\x00-\x1f\x7f]/g, '_') || 'download';
      document.body.append(anchor);
      anchor.click();
      anchor.remove();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    },
    diagram(element, source) {
      // Reject resource/navigation syntax before Mermaid can create staging DOM.
      // The iframe CSP remains the final boundary for every diagram type.
      const resourceSyntax = /[a-z][a-z0-9+.-]*:\/\/|(?:data|blob|file|mailto|javascript):|url\s*\(|@import|^\s*(?:click|link)\s/im;
      if (source.length > 50000 || /%%\s*\{|^\s*---/.test(source) || resourceSyntax.test(source)) {
        element.textContent = 'Diagram preview unavailable; view source below.';
        return () => {};
      }
      const frame = document.createElement('iframe');
      frame.title = 'Mermaid diagram';
      frame.setAttribute('sandbox', 'allow-scripts');
      frame.setAttribute('referrerpolicy', 'no-referrer');
      frame.style.cssText = 'display:block;width:100%;height:120px;border:0;color-scheme:dark';
      // No network, images, fonts, navigation or forms. Only our fetched local
      // renderer blob and fixed inline bootstrap are executable in this frame.
      const policy = `default-src 'none'; script-src 'unsafe-inline' blob:; style-src 'unsafe-inline'; img-src 'none'; connect-src 'none'; font-src 'none'; base-uri 'none'; form-action 'none'`;
      let cancelled = false;
      let deadline;
      const fail = () => {
        if (cancelled) return;
        cancelled = true;
        clearTimeout(deadline);
        removeEventListener('message', receive);
        element.textContent = 'Diagram preview unavailable; view source below.';
      };
      frame.srcdoc = `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${escape(policy)}"><style>html,body{margin:0;background:#191c1a;color:#ecebe3;font:14px sans-serif}#diagram{padding:8px;overflow:auto}svg{max-width:100%;height:auto}a{pointer-events:none}</style></head><body><div id="diagram" role="img" aria-label="Mermaid diagram" data-render-state="loading">Rendering diagram…</div><script>
        const box = document.getElementById('diagram');
        const reportHeight = () => parent.postMessage({demodexDiagramHeight: Math.ceil(box.scrollHeight), demodexDiagramState: box.dataset.renderState}, '*');
        new ResizeObserver(reportHeight).observe(box);
        addEventListener('message', async event => {
          if (event.source !== parent || typeof event.data?.source !== 'string') return;
          try {
          const script = document.createElement('script');
          const url = URL.createObjectURL(event.data.renderer);
          script.src = url;
          const finish = () => { URL.revokeObjectURL(url); reportHeight(); };
          script.onerror = () => { box.dataset.renderState = 'error'; box.textContent = 'Diagram preview unavailable; view source below.'; finish(); };
          script.onload = async () => {
          const stage = document.createElement('div');
          stage.style.cssText = 'position:absolute;visibility:hidden;inset:0 auto auto 0;width:100%;pointer-events:none';
          stage.setAttribute('aria-hidden','true');
          document.body.append(stage);
          try {
            mermaid.initialize({startOnLoad:false,securityLevel:'strict',theme:'dark',htmlLabels:false,suppressErrorRendering:true,maxTextSize:50000,maxEdges:200,flowchart:{htmlLabels:false},secure:['securityLevel','startOnLoad','maxTextSize','maxEdges','htmlLabels','themeCSS','dompurifyConfig']});
            const result = await mermaid.render('diagram-svg',event.data.source,stage);
            box.innerHTML = result.svg;
            // Strict Mermaid sanitizes SVG; remove navigation and foreign HTML
            // as well. The frame has neither same-origin access nor networking.
            box.querySelectorAll('a,foreignObject,image,script').forEach(node => node.remove());
            box.querySelectorAll('*').forEach(node => {for (const attr of [...node.attributes]) {if (attr.name.startsWith('on') || attr.name === 'href' || attr.name === 'xlink:href') node.removeAttribute(attr.name);}});
            box.dataset.renderState = 'complete';
          } catch (_) { box.dataset.renderState = 'error'; box.textContent = 'Diagram preview unavailable; view source below.'; }
          finally { stage.remove(); }
          finish();
          };
          document.head.append(script);
          } catch (_) { box.dataset.renderState = 'error'; box.textContent = 'Diagram preview unavailable; view source below.'; reportHeight(); }
        }, {once:true});
        parent.postMessage({demodexDiagramReady:true}, '*');
      <\/script></body></html>`;
      const receive = event => {
        if (event.source !== frame.contentWindow) return;
        if (event.data?.demodexDiagramReady) localRenderer().then(renderer => {
          if (!cancelled) frame.contentWindow.postMessage({source, renderer}, '*');
        }).catch(fail);
        const state = event.data?.demodexDiagramState;
        if (state === 'complete' || state === 'error') {
          frame.dataset.renderState = state;
          clearTimeout(deadline);
        }
        const height = event.data?.demodexDiagramHeight;
        if (Number.isFinite(height)) frame.style.height = `${Math.max(80, Math.min(2000, height))}px`;
      };
      addEventListener('message', receive);
      element.replaceChildren(frame);
      deadline = setTimeout(fail, 30000);
      return () => { cancelled = true; clearTimeout(deadline); removeEventListener('message', receive); frame.remove(); };
    }
  };
})();
