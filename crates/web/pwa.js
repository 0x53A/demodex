

window.demodexPwaState = { available: false, applying: false, error: '' };
const pwa = { update(fn) { window.demodexPwaState = fn(window.demodexPwaState); window.dispatchEvent(new CustomEvent('demodex-update-state')); } };
let registration;
let requested = false;
let reloading = false;
let changedElsewhere = false;

function applyUpdate() {
  if (requested) return;
  if (!registration?.waiting && !changedElsewhere) return;
  requested = true;
  pwa.update(state => ({ ...state, applying: true, error: '' }));
  if (registration?.waiting) {
    registration.waiting.postMessage({ type: 'ACTIVATE_UPDATE' });
    window.setTimeout(() => {
      if (!reloading) {
        requested = false;
        pwa.update(state => ({ ...state, applying: false, error: 'Update did not activate. Please try again.' }));
      }
    }, 10_000);
  } else location.reload();
}

// Pict-style startup: bounded automatic update before mounting the interactive
// UI. Later updates download automatically but require an explicit reload.
async function preparePwa() {
  if (!isSecureContext || !('serviceWorker' in navigator)) return true;
  let starting = true;
  let hadController = !!navigator.serviceWorker.controller;
  navigator.serviceWorker.addEventListener('controllerchange', () => {
    if (requested && !reloading) {
      reloading = true;
      location.reload();
    } else if (hadController) {
      changedElsewhere = true;
      pwa.update(state => ({ ...state, available: true }));
    }
    hadController = true;
  });
  const setup = (async () => {
    registration = await navigator.serviceWorker.register(new URL('./service-worker.js', document.baseURI), { updateViaCache: 'none' });
    const ready = () => {
      if (!registration?.waiting || !navigator.serviceWorker.controller) return;
      pwa.update(state => ({ ...state, available: true }));
      if (starting) applyUpdate();
    };
    const watch = (worker) => worker?.addEventListener('statechange', ready);
    watch(registration.installing);
    registration.addEventListener('updatefound', () => watch(registration.installing));
    ready();
    let checking = false;
    const check = async () => {
      if (checking || document.visibilityState !== 'visible') return;
      checking = true;
      try { await registration.update(); ready(); }
      catch { /* Offline: keep the installed shell and retry on foreground. */ }
      finally { checking = false; }
    };
    window.setInterval(() => void check(), 60_000);
    document.addEventListener('visibilitychange', () => void check());
    window.addEventListener('pageshow', () => void check());
    window.addEventListener('online', () => void check());
    await check();
    const worker = registration.installing;
    if (worker && !['installed', 'redundant'].includes(worker.state)) {
      await new Promise(resolve => worker.addEventListener('statechange', () => {
        if (['installed', 'redundant'].includes(worker.state)) resolve();
      }));
    }
    ready();
  })().catch(() => pwa.update(state => ({ ...state, error: 'Automatic updates are unavailable. Reload to check for a new version.' })));
  await Promise.race([setup, new Promise(resolve => window.setTimeout(resolve, 4000))]);
  starting = false;
  // A failed activation must fall back to the UI instead of leaving a blank page.
  if (requested && !reloading) await new Promise(resolve => window.setTimeout(resolve, 11_000));
  return !reloading;
}

window.demodexApplyUpdate = applyUpdate;
window.demodexPwaReady = preparePwa();
