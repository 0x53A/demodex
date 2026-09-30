let player;
const mounts = new WeakMap();

export function unmountScore(host) {
  mounts.get(host)?.cancel?.();
  mounts.delete(host);
  host.replaceChildren();
}

// This module contains only host glue. The synthesizer is initialized on demand.
async function loadPlayer() {
  if (!player) {
    player = (async () => {
      const response = await fetch(new URL('apteronotus.json', document.baseURI), { credentials: 'omit' });
      if (!response.ok || !response.headers.get('content-type')?.includes('application/json')) {
        throw new Error('This Demodex build does not include the Apteronotus player.');
      }
      const { module } = await response.json();
      if (typeof module !== 'string') throw new Error('Invalid Apteronotus player manifest.');
      const url = new URL(module, document.baseURI);
      if (url.origin !== location.origin) throw new Error('The player must be bundled with Demodex.');
      const app = await import(url.href);
      await app.default();
    })().catch(error => { player = undefined; throw error; });
  }
  return player;
}

export async function mountScore(host, source, filename) {
  unmountScore(host);
  const request = {};
  mounts.set(host, request);
  await loadPlayer();
  // Closing the Yew component during loading must not start a detached app.
  if (!host.isConnected || mounts.get(host) !== request) return;
  // The component library assigns its instance-ID attribute in the constructor.
  // Upgrade a parser-created element: createElement's synchronous-construction
  // checks forbid a custom constructor from adding attributes. This template
  // is constant; source text always enters through setAttribute below.
  const template = document.createElement('template');
  template.innerHTML = '<apteronotus-app></apteronotus-app>';
  const element = template.content.firstElementChild;
  element.style.cssText = 'display:block;width:100%;height:100%';
  element.setAttribute('filename', filename);
  element.setAttribute('source', source);
  await new Promise((resolve, reject) => {
    request.cancel = resolve;
    element.addEventListener('apteronotus-ready', resolve, { once: true });
    element.addEventListener('apteronotus-error', event => reject(new Error(event.detail)), { once: true });
    host.replaceChildren(element);
  }).catch(error => {
    // A rejected source can still initialize the component's starter editor.
    // Disconnect it, but preserve any newer mount that replaced this request.
    if (mounts.get(host) === request) unmountScore(host);
    throw error;
  });
}
