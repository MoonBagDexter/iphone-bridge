// iphone-bridge web client -- v1 (audio out + mic in, toggle-mode)

const audioBtn = document.getElementById('audio-btn');
const audioState = document.getElementById('audio-state');
const micBtn = document.getElementById('mic-btn');
const micState = document.getElementById('mic-state');
const toastEl = document.getElementById('toast');

// Toast: brief overlay messages. Replaces the old footer text area so the
// layout never has to make room for variable-length status strings.
let toastTimer = null;
function toast(msg, duration = 2500) {
  if (!msg) return;
  toastEl.textContent = msg;
  toastEl.classList.add('show');
  if (toastTimer) clearTimeout(toastTimer);
  toastTimer = setTimeout(() => toastEl.classList.remove('show'), duration);
}

// Audio-out state.
let audioCtx = null;
let masterGain = null;
let workletNode = null;
let ws = null;
let format = { sampleRate: 48000, channels: 2 };
let bytesIn = 0;
let lastStatsAt = 0;
let lastCushionMs = 0;
let lastUnderruns = 0;
let silentAnchor = null;

// Mic-in state.
let micCtx = null;
let micStream = null;
let micCaptureNode = null;
let micWs = null;
let micBytesOut = 0;

function setAudioState(running, msg) {
  audioBtn.classList.toggle('on', running);
  audioState.textContent = msg;
}
function setMicState(running, msg) {
  micBtn.classList.toggle('on', running);
  micState.textContent = msg;
}
function setFooter(msg) {
  // The footer element is gone -- redirect messages through the toast. Stats
  // updates (kbps / buffer ms) are filtered out to avoid spam; only real
  // notifications surface.
  if (!msg) return;
  if (/^\d+\s+kbps/.test(msg)) return; // skip routine stats lines
  toast(msg);
}

// ---- iOS silent-switch workaround (audio-out path only) -------------------

function makeSilentWavUrl(seconds = 1) {
  const sampleRate = 22050;
  const numSamples = sampleRate * seconds;
  const dataSize = numSamples * 2;
  const buf = new ArrayBuffer(44 + dataSize);
  const v = new DataView(buf);
  v.setUint32(0, 0x52494646, false);
  v.setUint32(4, 36 + dataSize, true);
  v.setUint32(8, 0x57415645, false);
  v.setUint32(12, 0x666d7420, false);
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true);
  v.setUint16(22, 1, true);
  v.setUint32(24, sampleRate, true);
  v.setUint32(28, sampleRate * 2, true);
  v.setUint16(32, 2, true);
  v.setUint16(34, 16, true);
  v.setUint32(36, 0x64617461, false);
  v.setUint32(40, dataSize, true);
  return URL.createObjectURL(new Blob([buf], { type: 'audio/wav' }));
}

function startSilentAnchor() {
  if (silentAnchor) return;
  silentAnchor = new Audio();
  silentAnchor.src = makeSilentWavUrl(1);
  silentAnchor.loop = true;
  silentAnchor.playsInline = true;
  silentAnchor.muted = false;
  silentAnchor.volume = 1.0;
  silentAnchor.play().catch((e) => console.warn('silent anchor play failed:', e));
}

function stopSilentAnchor() {
  if (!silentAnchor) return;
  silentAnchor.pause();
  silentAnchor.src = '';
  silentAnchor = null;
}

// ---- Beeps ----------------------------------------------------------------

function playOnTone() {
  const ctx = audioCtx;
  if (!ctx || !masterGain) return;
  const osc = ctx.createOscillator();
  const gain = ctx.createGain();
  osc.frequency.value = 440;
  gain.gain.setValueAtTime(0.0001, ctx.currentTime);
  gain.gain.exponentialRampToValueAtTime(0.2, ctx.currentTime + 0.02);
  gain.gain.exponentialRampToValueAtTime(0.0001, ctx.currentTime + 0.35);
  osc.connect(gain).connect(masterGain);
  osc.start(ctx.currentTime);
  osc.stop(ctx.currentTime + 0.4);
}

function playOffTone() {
  const ctx = audioCtx;
  if (!ctx) return;
  const osc = ctx.createOscillator();
  const gain = ctx.createGain();
  osc.frequency.value = 220;
  gain.gain.setValueAtTime(0.0001, ctx.currentTime);
  gain.gain.exponentialRampToValueAtTime(0.2, ctx.currentTime + 0.02);
  gain.gain.exponentialRampToValueAtTime(0.0001, ctx.currentTime + 0.35);
  osc.connect(gain).connect(ctx.destination);
  osc.start(ctx.currentTime);
  osc.stop(ctx.currentTime + 0.4);
}

// ---- Audio out (PC -> iPhone) --------------------------------------------

async function startAudio() {
  setAudioState(false, 'connecting…');
  try {
    startSilentAnchor();

    audioCtx = new (window.AudioContext || window.webkitAudioContext)({
      latencyHint: 'interactive',
      sampleRate: format.sampleRate,
    });
    await audioCtx.resume();

    masterGain = audioCtx.createGain();
    masterGain.gain.value = 1.0;
    masterGain.connect(audioCtx.destination);

    await audioCtx.audioWorklet.addModule('/worklet.js');
    workletNode = new AudioWorkletNode(audioCtx, 'stream-player', {
      numberOfInputs: 0,
      numberOfOutputs: 1,
      outputChannelCount: [2],
    });
    workletNode.port.onmessage = (e) => {
      const d = e.data;
      if (d.type === 'stats') {
        lastCushionMs = d.cushionMs;
        lastUnderruns = d.underruns;
      }
    };
    workletNode.connect(masterGain);

    playOnTone();
  } catch (e) {
    setAudioState(false, 'audio init failed');
    setFooter(String(e));
    return;
  }

  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  ws = new WebSocket(`${proto}//${location.host}/audio`);
  ws.binaryType = 'arraybuffer';

  ws.onopen = () => {
    bytesIn = 0;
    lastStatsAt = performance.now();
    setAudioState(true, 'streaming');
  };

  ws.onmessage = (e) => {
    if (typeof e.data === 'string') {
      try {
        const m = JSON.parse(e.data);
        if (m.type === 'format') {
          format.sampleRate = m.sampleRate;
          format.channels = m.channels;
        }
      } catch (_) { /* ignore */ }
      return;
    }
    if (!(e.data instanceof ArrayBuffer)) return;
    bytesIn += e.data.byteLength;
    if (workletNode) {
      const samples = new Float32Array(e.data);
      workletNode.port.postMessage({ type: 'pcm', samples }, [samples.buffer]);
    }
    maybeLogStats();
  };

  ws.onclose = () => {
    setAudioState(false, 'disconnected');
    teardownAudio();
  };

  ws.onerror = () => {
    setFooter('websocket error');
  };
}

function teardownAudio() {
  if (workletNode) {
    try { workletNode.disconnect(); } catch (_) {}
    workletNode = null;
  }
  if (audioCtx) {
    audioCtx.close().catch(() => {});
    audioCtx = null;
  }
  masterGain = null;
  ws = null;
  stopSilentAnchor();
}

async function stopAudio() {
  setAudioState(false, 'stopping…');

  if (ws) {
    try { ws.close(); } catch (_) {}
    ws = null;
  }

  if (audioCtx && masterGain) {
    try {
      playOffTone();
      const now = audioCtx.currentTime;
      masterGain.gain.cancelScheduledValues(now);
      masterGain.gain.setValueAtTime(masterGain.gain.value, now);
      masterGain.gain.linearRampToValueAtTime(0.0001, now + 0.18);
      await new Promise((r) => setTimeout(r, 340));
    } catch (_) { /* ignore */ }
  }

  teardownAudio();
  setAudioState(false, 'tap to start');
}

// ---- Mic in (iPhone -> PC, into VB-CABLE Input) ---------------------------

async function startMic() {
  setMicState(false, 'asking permission…');
  try {
    micStream = await navigator.mediaDevices.getUserMedia({
      audio: {
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
        channelCount: 1,
        sampleRate: 48000,
      },
      video: false,
    });
  } catch (e) {
    setMicState(false, 'mic denied');
    setFooter(String(e));
    return;
  }

  setMicState(false, 'connecting…');
  try {
    micCtx = new (window.AudioContext || window.webkitAudioContext)({
      latencyHint: 'interactive',
      sampleRate: 48000,
    });
    await micCtx.resume();
    await micCtx.audioWorklet.addModule('/mic-worklet.js');
    const source = micCtx.createMediaStreamSource(micStream);
    micCaptureNode = new AudioWorkletNode(micCtx, 'mic-capture', {
      numberOfInputs: 1,
      numberOfOutputs: 0,
    });
    source.connect(micCaptureNode);
  } catch (e) {
    setMicState(false, 'audio init failed');
    setFooter(String(e));
    teardownMic();
    return;
  }

  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  micWs = new WebSocket(`${proto}//${location.host}/mic`);
  micWs.binaryType = 'arraybuffer';

  micWs.onopen = () => {
    micBytesOut = 0;
    setMicState(true, 'live');
  };

  micWs.onclose = () => {
    setMicState(false, 'disconnected');
    teardownMic();
  };
  micWs.onerror = () => {
    setFooter('mic websocket error');
  };

  // Pump captured PCM out as soon as the worklet hands it to us.
  micCaptureNode.port.onmessage = (e) => {
    const d = e.data;
    if (d && d.type === 'pcm' && micWs && micWs.readyState === WebSocket.OPEN) {
      micWs.send(d.samples.buffer);
      micBytesOut += d.samples.byteLength;
    }
  };
}

function teardownMic() {
  if (micCaptureNode) {
    try { micCaptureNode.disconnect(); } catch (_) {}
    micCaptureNode = null;
  }
  if (micCtx) {
    micCtx.close().catch(() => {});
    micCtx = null;
  }
  if (micStream) {
    micStream.getTracks().forEach((t) => t.stop());
    micStream = null;
  }
  micWs = null;
}

async function stopMic() {
  setMicState(false, 'stopping…');
  if (micWs) {
    try { micWs.close(); } catch (_) {}
    micWs = null;
  }
  teardownMic();
  setMicState(false, 'tap to start');
}

// ---- Stats footer ---------------------------------------------------------

function maybeLogStats() {
  const now = performance.now();
  if (now - lastStatsAt < 1000) return;
  const dt = (now - lastStatsAt) / 1000;
  const kbps = (bytesIn * 8 / 1000 / dt).toFixed(0);
  const underrunNote = lastUnderruns > 0 ? ` · ${lastUnderruns} underruns` : '';
  setFooter(`${kbps} kbps · buffer ${lastCushionMs} ms${underrunNote}`);
  bytesIn = 0;
  lastStatsAt = now;
}

// ---- Wiring (independent: audio and mic can run at the same time) --------

audioBtn.addEventListener('click', async () => {
  const isOn = audioCtx && audioCtx.state !== 'closed';
  if (isOn) await stopAudio(); else await startAudio();
});

micBtn.addEventListener('click', async () => {
  const isOn = micCtx && micCtx.state !== 'closed';
  if (isOn) await stopMic(); else await startMic();
});

// ---- Push-to-talk mode ----------------------------------------------------
// Single big button: first tap requests iOS mic permission and opens the
// /mic WebSocket (kept open for the whole session). Subsequent taps toggle
// transmission and send "ptt:start" / "ptt:stop" text frames on the same WS,
// guaranteeing strict ordering between the last PCM byte and the stop signal.
// The server taps Alt immediately on start, and on stop it waits for the
// WASAPI render queue + VB-CABLE to drain before tapping Alt -- so SuperWhisper
// sees the full tail of the user's speech before its hotkey ends listening.

const pttBtn = document.getElementById('ptt-btn');
const pttStateEl = document.getElementById('ptt-state');

let pttActivated = false;
let pttTransmitting = false;
let pttCtx = null;
let pttStream = null;
let pttCaptureNode = null;
let pttWs = null;
let pttDrainTimer = null;

function setPttUi(label, stateMsg, cls /* 'on' | 'draining' | null */) {
  pttBtn.textContent = label;
  pttBtn.classList.toggle('on', cls === 'on');
  pttBtn.classList.toggle('draining', cls === 'draining');
  pttStateEl.textContent = stateMsg;
}

// Open the keys/control WebSocket. Doesn't require any iOS permission, so we
// can do this the moment the user enters PTT mode (or restores it on load).
// Nav-key buttons just need this open to work; only PTT itself needs the mic.
function openPttWs() {
  if (pttWs && pttWs.readyState !== WebSocket.CLOSED) return;
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  pttWs = new WebSocket(`${proto}//${location.host}/mic`);
  pttWs.binaryType = 'arraybuffer';

  pttWs.onclose = () => {
    pttTransmitting = false;
    teardownPttMic();
    pttWs = null;
    if (!pttActivated) {
      setPttUi('Activate', 'disconnected', null);
    } else {
      setPttUi('Activate', 'disconnected', null);
      pttActivated = false;
    }
  };

  pttWs.onerror = () => {
    setFooter('mic websocket error');
  };
}

async function activatePtt() {
  setPttUi('…', 'asking permission…', null);
  try {
    pttStream = await navigator.mediaDevices.getUserMedia({
      audio: {
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
        channelCount: 1,
        sampleRate: 48000,
      },
      video: false,
    });
  } catch (e) {
    setPttUi('Activate', 'mic denied', null);
    setFooter(String(e));
    return;
  }

  try {
    pttCtx = new (window.AudioContext || window.webkitAudioContext)({
      latencyHint: 'interactive',
      sampleRate: 48000,
    });
    await pttCtx.resume();
    await pttCtx.audioWorklet.addModule('/mic-worklet.js');
    const source = pttCtx.createMediaStreamSource(pttStream);
    pttCaptureNode = new AudioWorkletNode(pttCtx, 'mic-capture', {
      numberOfInputs: 1,
      numberOfOutputs: 0,
    });
    source.connect(pttCaptureNode);
  } catch (e) {
    setPttUi('Activate', 'audio init failed', null);
    setFooter(String(e));
    teardownPttMic();
    return;
  }

  // Make sure the control WS is up (it usually is from mode entry).
  openPttWs();

  pttActivated = true;
  setPttUi('Push to Talk', 'tap to talk', null);

  // Forward PCM only while transmitting; otherwise drop on the floor.
  pttCaptureNode.port.onmessage = (e) => {
    if (!pttTransmitting) return;
    const d = e.data;
    if (d && d.type === 'pcm' && pttWs && pttWs.readyState === WebSocket.OPEN) {
      pttWs.send(d.samples.buffer);
    }
  };
}

function teardownPttMic() {
  if (pttDrainTimer) { clearTimeout(pttDrainTimer); pttDrainTimer = null; }
  if (pttCaptureNode) {
    try { pttCaptureNode.port.onmessage = null; } catch (_) {}
    try { pttCaptureNode.disconnect(); } catch (_) {}
    pttCaptureNode = null;
  }
  if (pttCtx) { pttCtx.close().catch(() => {}); pttCtx = null; }
  if (pttStream) { pttStream.getTracks().forEach((t) => t.stop()); pttStream = null; }
  pttActivated = false;
  pttTransmitting = false;
}

function teardownPtt() {
  teardownPttMic();
  if (pttWs) {
    try { pttWs.close(); } catch (_) {}
    pttWs = null;
  }
}

function startTransmitting() {
  if (pttWs && pttWs.readyState === WebSocket.OPEN) {
    pttWs.send('ptt:start');
  }
  pttTransmitting = true;
  setPttUi('Listening…', 'release / tap to stop', 'on');
}

function stopTransmitting() {
  pttTransmitting = false;
  if (pttWs && pttWs.readyState === WebSocket.OPEN) {
    pttWs.send('ptt:stop');
  }
  setPttUi('Push to Talk', 'draining…', 'draining');
  if (pttDrainTimer) clearTimeout(pttDrainTimer);
  pttDrainTimer = setTimeout(() => {
    pttDrainTimer = null;
    if (!pttTransmitting && pttActivated) {
      setPttUi('Push to Talk', 'tap or hold', null);
    }
  }, 1000);
}

// PTT input model: tap-to-toggle AND hold-to-talk in one button.
// - Press while idle  -> start transmitting immediately.
// - Release within HOLD_MS of press -> keep transmitting (acts like a tap toggle).
// - Release after HOLD_MS -> stop transmitting (hold-to-talk).
// - Press while already transmitting -> stop (toggle off).
// pointercancel is treated identically to pointerup so a system-stolen gesture
// still cleanly closes the transmission instead of leaving SuperWhisper hot.
const PTT_HOLD_MS = 250;
let pttPressOpenedTx = false;
let pttPressTime = 0;

function pttPressDown(e) {
  if (e.cancelable) e.preventDefault();
  try { pttBtn.setPointerCapture(e.pointerId); } catch (_) {}
  if (!pttActivated) {
    activatePtt();
    return;
  }
  if (!pttTransmitting) {
    pttPressOpenedTx = true;
    pttPressTime = Date.now();
    startTransmitting();
  } else {
    // Pressing while already transmitting = toggle off (legacy tap behavior).
    pttPressOpenedTx = false;
    stopTransmitting();
  }
}

function pttPressEnd() {
  if (!pttActivated || !pttTransmitting || !pttPressOpenedTx) return;
  const held = Date.now() - pttPressTime;
  pttPressOpenedTx = false;
  if (held >= PTT_HOLD_MS) {
    // Real hold -> release stops transmission.
    stopTransmitting();
  }
  // Otherwise the user just tapped quickly; leave the transmission live so
  // the next tap toggles it off.
}

pttBtn.addEventListener('pointerdown', pttPressDown);
pttBtn.addEventListener('pointerup', pttPressEnd);
pttBtn.addEventListener('pointercancel', pttPressEnd);

// ---- Nav controls (arrows + editing + tab/desktop switching) -------------
// All key commands ride on the same /mic WebSocket as text frames. They are
// no-ops until the user has tapped Activate (since that's when pttWs opens).

function sendKey(name) {
  if (pttWs && pttWs.readyState === WebSocket.OPEN) {
    pttWs.send('key:' + name);
  }
}

// Reliable visual flash on every button press: iOS Safari's :active is flaky
// on rapid taps, so we add a `.flash` class via pointerdown for ~120 ms.
document.addEventListener('pointerdown', (e) => {
  const btn = e.target.closest('button');
  if (!btn) return;
  btn.classList.add('flash');
  setTimeout(() => btn.classList.remove('flash'), 130);
}, { passive: true });

// Hold-to-repeat: fires immediately on press, then after a 400 ms delay starts
// repeating every 50 ms until release. Used for arrows + backspace where the
// user wants to "hold to keep going" the way a physical keyboard does.
const HOLD_DELAY_MS = 400;
const HOLD_INTERVAL_MS = 55;
function bindHoldRepeat(btn, name) {
  let delayTimer = null;
  let repeatTimer = null;
  const start = (e) => {
    if (e.cancelable) e.preventDefault();
    sendKey(name);
    delayTimer = setTimeout(() => {
      delayTimer = null;
      repeatTimer = setInterval(() => sendKey(name), HOLD_INTERVAL_MS);
    }, HOLD_DELAY_MS);
  };
  const stop = () => {
    if (delayTimer) { clearTimeout(delayTimer); delayTimer = null; }
    if (repeatTimer) { clearInterval(repeatTimer); repeatTimer = null; }
  };
  btn.addEventListener('pointerdown', start);
  btn.addEventListener('pointerup', stop);
  btn.addEventListener('pointerleave', stop);
  btn.addEventListener('pointercancel', stop);
}

bindHoldRepeat(document.getElementById('key-up'), 'up');
bindHoldRepeat(document.getElementById('key-down'), 'down');
bindHoldRepeat(document.getElementById('key-left'), 'left');
bindHoldRepeat(document.getElementById('key-right'), 'right');
bindHoldRepeat(document.getElementById('key-backspace'), 'backspace');
// Ctrl+W is "delete word back" -- natural to hold like backspace, but word-level.
bindHoldRepeat(document.getElementById('key-ctrl-w'), 'ctrl-w');
// Space repeats when held, matching physical-keyboard behavior.
bindHoldRepeat(document.getElementById('key-space'), 'space');

document.getElementById('key-escape').addEventListener('click', () => sendKey('escape'));
document.getElementById('key-enter').addEventListener('click', () => sendKey('enter'));
document.getElementById('key-desktop-left').addEventListener('click', () => sendKey('desktop-left'));
document.getElementById('key-desktop-right').addEventListener('click', () => sendKey('desktop-right'));
document.getElementById('key-alt-tab').addEventListener('click', () => sendKey('alt-tab'));
document.getElementById('key-ctrl-tab').addEventListener('click', () => sendKey('ctrl-tab'));
document.getElementById('key-shift-tab').addEventListener('click', () => sendKey('shift-tab'));
document.getElementById('key-ctrl-u').addEventListener('click', () => sendKey('ctrl-u'));
document.getElementById('key-ctrl-k').addEventListener('click', () => sendKey('ctrl-k'));
document.getElementById('key-ctrl-a').addEventListener('click', () => sendKey('ctrl-a'));
document.getElementById('key-ctrl-e').addEventListener('click', () => sendKey('ctrl-e'));
document.getElementById('key-btw').addEventListener('click', () => sendKey('btw'));
document.getElementById('key-push').addEventListener('click', () => sendKey('push'));
document.getElementById('key-min-window').addEventListener('click', () => sendKey('min-window'));

// ---- Mode switcher --------------------------------------------------------

const modeBridgeBtn = document.getElementById('mode-bridge');
const modePttBtn = document.getElementById('mode-ptt');
const viewBridge = document.getElementById('view-bridge');
const viewPtt = document.getElementById('view-ptt');

const MODE_KEY = 'iphone-bridge-mode';

async function setMode(mode) {
  if (mode === 'ptt') {
    // Tear down Bridge resources if running.
    if (audioCtx && audioCtx.state !== 'closed') await stopAudio();
    if (micCtx && micCtx.state !== 'closed') await stopMic();

    viewBridge.hidden = true;
    viewPtt.hidden = false;
    modeBridgeBtn.classList.remove('active');
    modePttBtn.classList.add('active');

    // Open the control WebSocket immediately so nav keys (arrows, Esc,
    // Enter, Ctrl-X shortcuts, tab/desktop switching) work without the user
    // having to tap Activate first. The mic still requires Activate.
    openPttWs();
  } else {
    // Tear down PTT resources. If currently transmitting, send a stop so the
    // server doesn't leave SuperWhisper in the "listening" state.
    if (pttTransmitting && pttWs && pttWs.readyState === WebSocket.OPEN) {
      try { pttWs.send('ptt:stop'); } catch (_) {}
    }
    teardownPtt();
    setPttUi('Activate', 'tap to activate', null);

    viewBridge.hidden = false;
    viewPtt.hidden = true;
    modeBridgeBtn.classList.add('active');
    modePttBtn.classList.remove('active');
  }
  try { localStorage.setItem(MODE_KEY, mode); } catch (_) { /* private mode */ }
}

modeBridgeBtn.addEventListener('click', () => setMode('bridge'));
modePttBtn.addEventListener('click', () => setMode('ptt'));

// Restore last-used mode on load.
try {
  const saved = localStorage.getItem(MODE_KEY);
  if (saved === 'ptt') setMode('ptt');
} catch (_) { /* ignore */ }
