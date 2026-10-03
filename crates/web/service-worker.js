// SHARE_INBOX
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
  if (request.method === 'POST' && url.href === new URL('./share-target', root).href) {
    event.respondWith((async () => {
      const destination = new URL(root);
      try { destination.searchParams.set('share', await demodexShares.receive(request)); }
      catch (error) { destination.searchParams.set('share_error', error.message || 'Could not receive shared content'); }
      return Response.redirect(destination.href, 303);
    })());
    return;
  }
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

// Push settings are separate from the static asset cache; no transcript or token
// enters worker storage. A subscription is bound to one daemon identity.
async function pushBinding() {
  const db=await new Promise((resolve,reject)=>{const r=indexedDB.open(`demodex-push:${new URL(self.registration.scope).pathname}`,1);r.onupgradeneeded=()=>r.result.createObjectStore('settings');r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error);});
  try{return await new Promise((resolve,reject)=>{const r=db.transaction('settings').objectStore('settings').get('binding');r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error);});}finally{db.close();}
}
self.addEventListener('push',event=>event.waitUntil((async()=>{
  let data;try{data=event.data?.json();}catch{return;}
  const binding=await pushBinding();
  if(!binding || data?.server_id!==binding.server_id || data?.server_url!==binding.server_url)return;
  if(typeof data.title!=='string'||typeof data.message!=='string'||typeof data.id!=='string')return;
  const url=new URL(self.registration.scope);
  url.searchParams.set('notify_server',binding.server_url);
  url.searchParams.set('notify_session',typeof data.session_id==='string'?data.session_id:'');
  await self.registration.showNotification(data.title.slice(0,240),{
    body:data.message.slice(0,1500),tag:`demodex:${data.server_id}:${data.id}`,renotify:false,
    icon:new URL('./icon-192.png',self.registration.scope).href,
    data:{url:url.href},
  });
})()));
self.addEventListener('notificationclick',event=>{
  event.notification.close();
  event.waitUntil((async()=>{
    const url=new URL(event.notification.data?.url||self.registration.scope);
    const scope=new URL(self.registration.scope);
    if(url.origin!==scope.origin||url.pathname!==scope.pathname)return;
    // A new client keeps an already-open conversation and its unsent draft intact.
    await self.clients.openWindow(url.href);
  })());
});
