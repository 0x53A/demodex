// Temporary, scope-local inbound shares. No credentials or conversation cache.
// Also included verbatim in the sealed worker by build-web.py.
(() => {
  const scope = typeof document === 'undefined' ? self.registration.scope : new URL('./', document.baseURI).href;
  const dbName = `demodex-shares:${scope}`;
  const maxFile = 32 * 1024 * 1024, maxTotal = 64 * 1024 * 1024;
  const expiry = 24 * 60 * 60 * 1000;
  async function transaction(mode, work) {
    const db = await new Promise((resolve, reject) => {
      const r = indexedDB.open(dbName, 1);
      r.onupgradeneeded = () => r.result.createObjectStore('inbox', {keyPath: 'id'});
      r.onsuccess = () => resolve(r.result); r.onerror = () => reject(r.error);
    });
    try {
      return await new Promise((resolve, reject) => {
        const tx = db.transaction('inbox', mode), store = tx.objectStore('inbox');
        let result;
        work(store, value => { result = value; });
        tx.oncomplete = () => resolve(result);
        tx.onerror = tx.onabort = () => reject(tx.error || new Error('Could not save shared content'));
      });
    } finally { db.close(); }
  }
  async function load(id) {
    return transaction('readwrite', (store, done) => {
      const r = store.get(id);
      r.onsuccess = () => {
        const item = r.result;
        if (item && Date.now() - item.created > expiry) { store.delete(id); done(null); }
        else done(item || null);
      };
    });
  }
  async function update(id, change) {
    return transaction('readwrite', (store, done) => {
      const r = store.get(id);
      r.onsuccess = () => {
        if (!r.result) { done(null); return; }
        const item = r.result;
        if (!change(item)) { done(null); return; }
        store.put(item); done(item);
      };
    });
  }
  globalThis.demodexShares = {
    async receive(request) {
      const form = await request.formData();
      const fields = ['title', 'text', 'url'].map(key => {
        const v = form.get(key); return typeof v === 'string' ? v : '';
      });
      if (fields.reduce((n, s) => n + s.length, 0) > 256 * 1024) throw new Error('Shared text exceeds 256 KiB');
      const files = form.getAll('files').filter(v => typeof v !== 'string');
      if (files.length > 10 || files.some(f => f.size > maxFile) || files.reduce((n, f) => n + f.size, 0) > maxTotal)
        throw new Error('Share up to 10 files, 32 MiB per file and 64 MiB total');
      if (files.some(f => !f.name || new TextEncoder().encode(f.name).length > 240 || /[\x00-\x1f\x7f/\\]/.test(f.name) || ['.', '..'].includes(f.name)))
        throw new Error('A shared filename is invalid');
      if (!files.length && !fields.some(s => s.length)) throw new Error('Nothing was shared');
      const item = {id: crypto.randomUUID(), created: Date.now(), fields, files, status: 'pending', uploads: []};
      await transaction('readwrite', (store, done) => {
        const r = store.getAll();
        r.onsuccess = () => {
          const live = r.result.filter(v => Date.now() - v.created <= expiry);
          for (const old of r.result) if (!live.includes(old)) store.delete(old.id);
          if (live.length >= 10 || live.reduce((n, v) => n + v.files.reduce((m, f) => m + f.size, 0), 0) + files.reduce((n, f) => n + f.size, 0) > 128 * 1024 * 1024) {
            done(false); return;
          }
          store.put(item); done(true);
        };
      }).then(ok => { if (!ok) throw new Error('Share inbox is full. Finish or discard earlier shares first'); });
      return item.id;
    },
    load,
    route() { return new URL(location.href).searchParams.get('share'); },
    clearRoute() {
      const url = new URL(location.href); url.searchParams.delete('share'); url.searchParams.delete('share_error');
      history.replaceState(history.state, '', url);
    },
    error() { return new URL(location.href).searchParams.get('share_error') || ''; },
    begin(id, host, session) {
      return update(id, item => {
        if (item.status !== 'pending') return false;
        item.status = 'uploading'; item.host = host; item.session = session;
        item.uploads = item.files.map(() => ({receipt: crypto.randomUUID()}));
        return true;
      });
    },
    progress(id, index, path) {
      return update(id, item => { item.uploads[index].path = path; return true; });
    },
    finish(id, error) {
      return update(id, item => { item.status = error ? 'failed' : 'ready'; item.error = error; return true; });
    },
    remove(id) { return transaction('readwrite', (store) => { store.delete(id); }); },
  };
})();
