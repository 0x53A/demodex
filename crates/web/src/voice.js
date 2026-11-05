// Microphone PCM only. No browser/cloud speech recognition API is used.
const processor = `
class Capture extends AudioWorkletProcessor {
  constructor() {
    super(); this.pending = []; this.offset = 16; this.chunk = [];
    this.stopped = false; this.total = 0;
    this.port.onmessage = () => {
      this.stopped = true;
      if (this.chunk.length) this.port.postMessage(new Int16Array(this.chunk));
      this.chunk = []; this.port.postMessage('stopped');
    };
  }
  process(inputs) {
    if (this.stopped) return false;
    const input = inputs[0]?.[0];
    if (!input) return true;
    this.pending.push(...input);
    const step = sampleRate / 16000;
    // Windowed-sinc low pass: avoid aliasing when devices capture at 44.1/48 kHz.
    const cutoff = Math.min(1, 16000 / sampleRate) * 0.90;
    while (this.offset + 16 < this.pending.length) {
      let sum = 0, norm = 0;
      const center = Math.floor(this.offset);
      for (let i = center - 15; i <= center + 16; i++) {
        const x = i - this.offset;
        const sinc = Math.abs(x) < 1e-8 ? cutoff : Math.sin(Math.PI * cutoff * x) / (Math.PI * x);
        const w = sinc * (0.5 + 0.5 * Math.cos(Math.PI * x / 16));
        sum += (this.pending[i] || 0) * w; norm += w;
      }
      const v = Math.max(-1, Math.min(1, sum / norm));
      this.chunk.push(Math.round(v < 0 ? v * 32768 : v * 32767));
      this.offset += step;
      if (++this.total === 16000 * 180) {
        this.port.postMessage(new Int16Array(this.chunk)); this.chunk = [];
        this.stopped = true; this.port.postMessage('limit'); return false;
      }
      if (this.chunk.length === 4000) {
        this.port.postMessage(new Int16Array(this.chunk)); this.chunk = [];
      }
    }
    const discard = Math.max(0, Math.floor(this.offset) - 16);
    this.pending.splice(0, discard); this.offset -= discard;
    return true;
  }
}
registerProcessor('demodex-capture', Capture);
`;
export async function prepareRecording(callback, warning = () => {}) {
  if (!navigator.mediaDevices?.getUserMedia) throw new Error('Microphone access requires HTTPS and a supported browser');
  const stream = await navigator.mediaDevices.getUserMedia({audio: {channelCount: 1, echoCancellation: true, noiseSuppression: true}, video: false});
  let context, node, source, url, timer, stopResolve;
  let closed = false, deliberate = false, wakeLock;
  const releaseWakeLock = lock => { void lock.release().catch(() => {}); };
  const keepScreenOn = async () => {
    try {
      if (!navigator.wakeLock?.request) throw new Error('Wake lock unavailable');
      const lock = await navigator.wakeLock.request('screen');
      // Stop/cancel may complete while the browser is deciding the request.
      if (closed) { releaseWakeLock(lock); return; }
      wakeLock = lock;
      const released = () => {
        if (!closed) warning('Screen may turn off. Keep it awake while recording.');
      };
      lock.addEventListener('release', released, {once: true});
      if (lock.released) released();
    } catch (_) {
      if (!closed) warning('Cannot keep the screen on. Keep it awake while recording.');
    }
  };
  const release = () => {
    if (closed) return;
    closed = true; clearTimeout(timer);
    if (wakeLock) { releaseWakeLock(wakeLock); wakeLock = undefined; }
    stream.getTracks().forEach(track => track.stop());
    if (node) { node.port.onmessage = null; node.port.close(); node.onprocessorerror = null; node.disconnect(); }
    source?.disconnect();
    if (context) void context.close();
    if (url) URL.revokeObjectURL(url);
    document.removeEventListener('visibilitychange', hidden);
    stopResolve?.();
  };
  const fail = message => { if (!closed && !deliberate) callback(message); release(); };
  const hidden = () => { if (document.hidden) fail('Recording cancelled because the page was hidden'); };
  try {
    context = new AudioContext();
    await context.resume();
    url = URL.createObjectURL(new Blob([processor], {type: 'text/javascript'}));
    await context.audioWorklet.addModule(url);
    node = new AudioWorkletNode(context, 'demodex-capture');
    source = context.createMediaStreamSource(stream);
    node.port.onmessage = ({data}) => {
      if (closed) return;
      if (data === 'stopped') { release(); return; }
      if (data === 'limit') { deliberate = true; callback('finish'); release(); return; }
      if (typeof data === 'string') { fail(data); return; }
      callback(data);
    };
    node.onprocessorerror = () => fail('Microphone audio processing failed');
    stream.getTracks().forEach(track => track.onended = () => fail('Microphone disconnected'));
    document.addEventListener('visibilitychange', hidden);
    const stopCapture = () => {
      if (closed) return Promise.resolve();
      deliberate = true;
      return new Promise(resolve => { stopResolve = resolve; node.port.postMessage('stop');
        clearTimeout(timer); timer = setTimeout(() => { callback('Microphone did not finish cleanly'); release(); }, 2000);
      });
    };
    return {
      start() {
        if (closed) throw new Error('Recording cancelled');
        source.connect(node); node.connect(context.destination);
        void keepScreenOn();
        timer = setTimeout(() => { void stopCapture().then(() => callback('finish')); }, 180000);
      },
      stop: stopCapture,
      cancel: release,
    };
  } catch (error) { release(); throw error; }
}
export function startRecording(controller) { controller.start(); }
export function stopRecording(controller) { return controller.stop(); }
export function cancelRecording(controller) { controller.cancel(); }
