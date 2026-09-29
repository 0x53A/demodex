/* Push subscription glue; all daemon operations go through authenticated Wormhole. */
(() => {
  const dbName = `demodex-push:${new URL('./', document.baseURI).pathname}`;
  async function binding(value) {
    const db = await new Promise((resolve, reject) => { const r=indexedDB.open(dbName,1);r.onupgradeneeded=()=>r.result.createObjectStore('settings');r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error); });
    try { return await new Promise((resolve,reject)=>{const tx=db.transaction('settings',value===undefined?'readonly':'readwrite');const store=tx.objectStore('settings');const r=value===undefined?store.get('binding'):store.put(value,'binding');let result;r.onsuccess=()=>result=r.result;tx.oncomplete=()=>resolve(result);tx.onerror=()=>reject(tx.error);}); }
    finally {db.close();}
  }
  const supported=()=>isSecureContext && 'serviceWorker' in navigator && 'PushManager' in window && 'Notification' in window;
  const ready=()=>Promise.race([navigator.serviceWorker.ready,new Promise((_,reject)=>setTimeout(()=>reject(Error('Service worker is not ready. Reload and try again.')),10000))]);
  async function status(){
    if(!supported())return {supported:false,permission:'unavailable'};
    const b=await binding();const reg=await ready();const sub=await reg.pushManager.getSubscription();
    return {supported:true,permission:Notification.permission,binding:b||null,subscribed:!!sub};
  }
  async function enable(options){
    if(!supported())throw Error('Push is unavailable. On iPhone or iPad, install Demodex on the Home Screen first.');
    // Keep the permission request directly inside the user's click handler.
    const permission=Notification.permission==='granted'?'granted':await Notification.requestPermission();
    if(permission!=='granted')throw Error('Notifications are blocked. Change the browser or device notification permission to enable them.');
    const old=await binding();
    if(old && old.server_id!==options.server_id)throw Error('Disable push for the previous server on this installation first.');
    const reg=await ready();let sub=await reg.pushManager.getSubscription();
    const key=Uint8Array.from(atob(options.public_key.replace(/-/g,'+').replace(/_/g,'/')),c=>c.charCodeAt(0));
    if(sub){const previous=new Uint8Array(sub.options.applicationServerKey||[]);if(previous.length!==key.length||previous.some((v,i)=>v!==key[i]))throw Error('Disable the old subscription before enabling this server.');}
    else sub=await reg.pushManager.subscribe({userVisibleOnly:true,applicationServerKey:key});
    const device_id=old?.device_id||crypto.randomUUID();
    const current={device_id,server_id:options.server_id,server_url:options.server_url,public_key:options.public_key};
    await binding(current);
    return {...sub.toJSON(),...current,frontend_url:new URL('./',document.baseURI).href};
  }
  async function disable(){
    const old=await binding();const reg=await ready();const sub=await reg.pushManager.getSubscription();
    if(sub && !await sub.unsubscribe())throw Error('The browser could not unsubscribe. Try again.');
    await binding(null);return old;
  }
  // A clicked notification uses only navigation metadata, never credentials.
  function route(){
    const u=new URL(location.href);const server=u.searchParams.get('notify_server');const session=u.searchParams.get('notify_session');
    if(!server)return null;
    let parsed;try{parsed=new URL(server);}catch{return null;}
    if(!['https:','http:'].includes(parsed.protocol)||parsed.username||parsed.password||parsed.search||parsed.hash||parsed.pathname!=='/')return null;
    u.searchParams.delete('notify_server');u.searchParams.delete('notify_session');history.replaceState(history.state,'',u);
    return {host:parsed.origin,selected:session||'',page:'',connections:false};
  }
  window.demodexPush={status,enable,disable,route};
})();
