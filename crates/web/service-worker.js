/* BUILD is injected by the production build, including each asset's digest. */
const prefix = `demodex-shell-${encodeURIComponent(self.registration.scope)}-`;
const cacheName = prefix + BUILD.version;
const currentCache = caches.open(cacheName);
const assetUrls = new Set(BUILD.assets.map(asset => new URL(asset.path, self.registration.scope).href));
const indexUrl = new URL('./index.html', self.registration.scope).href;

self.addEventListener('install', event => event.waitUntil((async () => {
  const cache = await currentCache;
  try {
    // Verify the entire release before making it available. A partial deployment
    // or an HTML fallback for a missing asset must not replace a working client.
    for (const asset of BUILD.assets) {
      const url = new URL(asset.path, self.registration.scope).href;
      const response = await fetch(url, { cache: 'reload', credentials: 'omit' });
      if (!response.ok || response.redirected) throw new Error(`Cannot cache ${asset.path}`);
      const bytes = await response.clone().arrayBuffer();
      const hash = [...new Uint8Array(await crypto.subtle.digest('SHA-256', bytes))]
        .map(byte => byte.toString(16).padStart(2, '0')).join('');
      if (hash !== asset.hash) throw new Error(`Release changed during download: ${asset.path}`);
      await cache.put(url, response);
    }
  } catch (error) {
    await caches.delete(cacheName);
    throw error;
  }
  // No skipWaiting here: a running page chooses when to apply an update.
})()));

self.addEventListener('activate', event => event.waitUntil((async () => {
  // Keep old hashed assets while another tab may still need them. Clean up when
  // activation happens with no open pages; never delete another app's caches.
  const pages = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
  if (pages.length === 0) {
    for (const key of await caches.keys()) if (key.startsWith(prefix) && key !== cacheName) await caches.delete(key);
  }
  await self.clients.claim();
})()));

self.addEventListener('fetch', event => {
  const request = event.request;
  const url = new URL(request.url);
  const root = new URL(self.registration.scope);
  if (request.method !== 'GET' || url.origin !== root.origin || request.headers.has('Authorization')) return;
  // API reads and all writes stay network-only. Nothing is queued for replay.
  if (url.pathname.startsWith(root.pathname + 'api/')) return;
  const navigation = request.mode === 'navigate' && url.pathname === root.pathname;
  if (navigation || assetUrls.has(url.href)) {
    event.respondWith((async () => (await (await currentCache).match(navigation ? indexUrl : request)) || fetch(request))());
  } else if (url.pathname.startsWith(root.pathname + 'assets/')) {
    // A tab deliberately remaining on an older release may request an old chunk.
    event.respondWith((async () => {
      for (const key of await caches.keys()) {
        if (!key.startsWith(prefix)) continue;
        const response = await (await caches.open(key)).match(request);
        if (response) return response;
      }
      return fetch(request);
    })());
  }
});

self.addEventListener('message', event => {
  if (event.data?.type === 'ACTIVATE_UPDATE') event.waitUntil(self.skipWaiting());
});
