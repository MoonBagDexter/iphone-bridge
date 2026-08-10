// iphone-bridge web client -- v1 (audio out + mic in, toggle-mode)

const audioBtn = document.getElementById('audio-btn');
const audioState = document.getElementById('audio-state');
const micBtn = document.getElementById('mic-btn');
const micState = document.getElementById('mic-state');
const toastEl = document.getElementById('toast');

// Toast: brief overlay messages. Replaces the old footer text area so the
// layout never has to make room for variable-length status strings.
let toastTimer = null;
// Turn a thrown value into something worth showing a person. DOM exceptions name
// internals ("NotAllowedError") and say nothing about what to do next, so the
// cases we actually hit get a plain-language line with the fix in it.
function friendlyError(err) {
  const name = (err && err.name) || '';
  const raw = String((err && err.message) || err || '').trim();
  switch (name) {
    case 'NotAllowedError':
      return 'Mic denied — allow in Settings › Safari';
    case 'NotFoundError':
      return 'No microphone found on this device';
    case 'NotReadableError':
      return 'Another app is using the mic';
    case 'AbortError':
      return 'That took too long — try again';
    default:
      break;
  }
  if (/Failed to fetch|NetworkError|Load failed/i.test(raw)) {
    return "Can't reach the PC — is the bridge still running?";
  }
  // Strip a leading "SomeError: " so a fallback still reads as a sentence.
  return raw.replace(/^[A-Za-z]*Error:\s*/, '') || 'Something went wrong';
}

function toast(msg, duration = 2500) {
  if (!msg) return;
  toastEl.textContent = msg;
  toastEl.classList.add('show');
  if (toastTimer) clearTimeout(toastTimer);
  toastTimer = setTimeout(() => toastEl.classList.remove('show'), duration);
}

// ---- Files tab: pure helpers (tested by tests/files-ui.test.mjs) ----------
// --- files-ui-pure (pure; tested by tests/files-ui.test.mjs) ---
// No DOM/globals in here -- same discipline as ptt-press-machine below.

// Splits an absolute Windows path into breadcrumb segments, using
// effectiveRoots to find the display name for the top-level segment. Picks
// the LONGEST matching root (so nested roots like C:\Users and
// C:\Users\thedi both being registered resolves to the more specific one).
// Falls back to the raw path as a single crumb if no root matches (path
// moved outside any configured root, or roots not loaded yet).
function filesBreadcrumbs(fullPath, effectiveRoots) {
  const norm = (p) => p.toLowerCase().replace(/\/+$/, '').replace(/\\+$/, '');
  const target = norm(fullPath);

  let bestRoot = null;
  for (const root of effectiveRoots || []) {
    const rootNorm = norm(root.path);
    if (target === rootNorm || target.startsWith(rootNorm + '\\')) {
      if (!bestRoot || rootNorm.length > norm(bestRoot.path).length) {
        bestRoot = root;
      }
    }
  }

  if (!bestRoot) {
    return [{ label: fullPath, path: fullPath }];
  }

  const crumbs = [{ label: bestRoot.name, path: bestRoot.path }];
  const rootNorm = norm(bestRoot.path);
  const remainder = fullPath.slice(bestRoot.path.length);
  const parts = remainder.split(/[\\/]+/).filter(Boolean);

  let cumulative = bestRoot.path;
  for (const part of parts) {
    cumulative = cumulative.replace(/[\\/]+$/, '') + '\\' + part;
    crumbs.push({ label: part, path: cumulative });
  }
  return crumbs;
}

// Returns a NEW recents array with `p` moved (or added) to the front,
// deduped case-insensitively (Windows paths), capped at `cap` entries.
function updateRecents(recents, p, cap = 8) {
  const norm = (x) => x.toLowerCase().replace(/[\\/]+$/, '');
  const target = norm(p);
  const rest = (recents || []).filter((r) => norm(r) !== target);
  return [p, ...rest].slice(0, cap);
}

// ---- Folder History (replaces the old recents chip row) ---------------------
// A history entry is { path, at } where `at` is the last-visited time in ms
// (null when unknown, e.g. a legacy recents entry that predates timestamps).

// Normalizes whatever's in storage into a clean { path, at }[] list. Prefers
// the new `files.history` shape; falls back to migrating the legacy
// `files.recents` shape (a plain array of path strings). Drops malformed
// entries so one bad record can't poison the list. Capped at `cap`.
function migrateHistory(storedHistory, legacyRecents, cap = 20) {
  if (Array.isArray(storedHistory)) {
    const clean = storedHistory.filter(
      (e) => e && typeof e === 'object' && typeof e.path === 'string'
    ).map((e) => ({ path: e.path, at: typeof e.at === 'number' ? e.at : null }));
    return clean.slice(0, cap);
  }
  if (Array.isArray(legacyRecents)) {
    return legacyRecents
      .filter((p) => typeof p === 'string')
      .map((p) => ({ path: p, at: null }))
      .slice(0, cap);
  }
  return [];
}

// Returns a NEW history list with `path` moved (or added) to the front, stamped
// with visit time `at`, deduped case-insensitively, capped at `cap`.
function updateHistory(history, path, at, cap = 20) {
  const norm = (x) => String(x).toLowerCase().replace(/[\\/]+$/, '');
  const target = norm(path);
  const rest = (history || []).filter((h) => norm(h.path) !== target);
  return [{ path, at }, ...rest].slice(0, cap);
}

// Display model for a history row: folder name (tail), dim parent path, and a
// relative last-visited label (reusing formatRelativeTime; '' when at is null).
function historyEntryModel(entry, now) {
  const p = String(entry.path);
  // Split on the LAST separator so a folder name repeated earlier in the path
  // can't shorten the parent. Drive roots (one segment) have no parent.
  const m = p.match(/^(.*)[\\/]+([^\\/]+)[\\/]*$/);
  const name = m ? m[2] : (p.split(/[\\/]+/).filter(Boolean).pop() || p);
  const parent = m ? m[1].replace(/[\\/]+$/, '') : '';
  return {
    path: entry.path,
    name,
    parent,
    ago: entry.at === null || entry.at === undefined ? '' : formatRelativeTime(entry.at, now),
  };
}

// Which primary surface each Files sub-view shows. Encodes the show/hide
// symmetry (exactly one of entryList/trash/history visible) that a past bug
// got wrong by leaving a sub-view's nodes on screen after leaving it. Unknown
// views degrade to the browser.
function subViewVisibility(view) {
  return {
    entryList: view !== 'trash' && view !== 'history',
    trash: view === 'trash',
    history: view === 'history',
  };
}

// Merges raw `ls` entries with the per-session gitstatus cache (path -> dirty
// bool, only populated once /api/gitstatus has been fetched for that path)
// and the favorites list, producing renderable entries annotated with
// `dirty` (true/false once known, null if unknown or not a repo) and
// `favorite` (bool). Never mutates its inputs.
function annotateEntries(entries, gitStatusCache, favorites) {
  const norm = (x) => x.toLowerCase().replace(/[\\/]+$/, '');
  const favSet = new Set((favorites || []).map(norm));
  const cache = gitStatusCache || {};
  return (entries || []).map((e) => {
    let dirty = null;
    if (e.isRepo && Object.prototype.hasOwnProperty.call(cache, e.path)) {
      dirty = cache[e.path];
    }
    return { ...e, dirty, favorite: favSet.has(norm(e.path)) };
  });
}
// Formats a deletedAtMs timestamp (ms since epoch, or null) relative to `now`
// for display in the Trash view: null -> em dash; <1min -> "just now";
// <1hr -> "Nm ago"; <24hr -> "Nh ago"; <=7 days -> "Nd ago"; beyond that,
// an absolute locale date string (so very old trash doesn't show a vague
// "40d ago").
function formatRelativeTime(deletedAtMs, now) {
  if (deletedAtMs === null || deletedAtMs === undefined) return '—';
  const diffMs = Math.max(0, now - deletedAtMs);
  const mins = Math.floor(diffMs / 60000);
  if (mins < 1) return 'just now';
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  if (days <= 7) return `${days}d ago`;
  return new Date(deletedAtMs).toLocaleDateString();
}

// Maps a raw /api/trash item into display-ready fields. Never mutates the
// input. sizeBytes null -> '' (no size shown); otherwise bytes/KB/MB with
// whole-number rounding, matching the coarse granularity a phone screen
// needs (no decimals).
function formatTrashItem(item, now) {
  let sizeLabel = '';
  if (item.sizeBytes !== null && item.sizeBytes !== undefined) {
    const b = item.sizeBytes;
    if (b < 1024) sizeLabel = `${b} B`;
    else if (b < 1024 * 1024) sizeLabel = `${Math.round(b / 1024)} KB`;
    else sizeLabel = `${Math.round(b / (1024 * 1024))} MB`;
  }
  return {
    id: item.id,
    name: item.name,
    originalPath: item.originalPath,
    isDir: item.isDir,
    sizeLabel,
    deletedLabel: formatRelativeTime(item.deletedAtMs, now),
  };
}

// Builds the "Quick access" chip data from the server's roots.shortcuts list,
// appending a trailing pseudo-chip for Trash (path: null, isTrash: true) so
// the renderer can style/route it differently. Empty shortcuts -> empty
// array, letting the caller hide the whole chip row (no Trash-only row).
function buildShortcutChips(shortcuts) {
  const chips = (shortcuts || []).map((s) => ({ name: s.name, path: s.path, isTrash: false }));
  chips.push({ name: 'Trash', path: null, isTrash: true });
  return chips;
}
// Builds the value to persist as the "last location" in localStorage after a
// successful navigation, so a refresh/reopen can restore exactly where the
// user left off. `view` is 'browser' (the folder browser, path may be null
// for the roots list) or 'trash' (the Trash view).
function nextLastLocation(view, path) {
  return { view, path: path === undefined ? null : path };
}

// Validates/normalizes a value read back from localStorage into a safe
// restore target. Anything malformed (not an object, bad view, non-string
// path) degrades gracefully to the roots view rather than throwing --
// storage can hold stale/garbage data from an older app version.
function resolveRestoreLocation(stored) {
  if (!stored || typeof stored !== 'object') return { view: 'browser', path: null };
  const view = stored.view === 'trash' ? 'trash' : 'browser';
  const path = typeof stored.path === 'string' ? stored.path : null;
  return { view, path };
}

// Comparator factory for the entry list. Directories always sort before files.
// Within a kind: 'modified' -> newest mtimeMs first; anything else ('name' or
// an unknown value) -> case-insensitive name ascending. Pure: returns a
// (a, b) => number suitable for Array.prototype.sort.
function filesSortComparator(mode) {
  const byName = (a, b) => {
    const an = String(a.name || '').toLowerCase();
    const bn = String(b.name || '').toLowerCase();
    if (an < bn) return -1;
    if (an > bn) return 1;
    return 0;
  };
  return (a, b) => {
    const ad = a.isDir ? 0 : 1;
    const bd = b.isDir ? 0 : 1;
    if (ad !== bd) return ad - bd;
    if (mode === 'modified') {
      const am = a.mtimeMs || 0;
      const bm = b.mtimeMs || 0;
      if (am !== bm) return bm - am; // newest first
      return byName(a, b); // stable-ish tiebreak
    }
    return byName(a, b);
  };
}

// Formats free/total disk space for a root row. null on either side -> ''
// (nothing shown). Uses GB with one decimal up to 1024 GB, TB with one decimal
// beyond that. Free and total are sized independently.
function formatDiskSpace(freeBytes, totalBytes) {
  if (freeBytes === null || freeBytes === undefined) return '';
  if (totalBytes === null || totalBytes === undefined) return '';
  const GB = 1024 * 1024 * 1024;
  const unit = (bytes) => {
    const gb = bytes / GB;
    if (gb >= 1024) return `${(gb / 1024).toFixed(1)} TB`;
    return `${gb.toFixed(1)} GB`;
  };
  return `${unit(freeBytes)} free of ${unit(totalBytes)}`;
}

// Maps a git short-status code (M/A/D/??/R/...) to a display model with a
// color bucket for the git-glance sheet. M amber, A green, D red, ?? gray;
// anything else (R, C, unknown) falls back to gray. label echoes the code.
function gitStatusMeta(status) {
  const code = String(status || '').trim();
  let color = 'gray';
  if (code === 'M') color = 'amber';
  else if (code === 'A') color = 'green';
  else if (code === 'D') color = 'red';
  else if (code === '??') color = 'gray';
  return { label: code, color };
}

// Maps a session-peek line {role, text} into a render model. user lines get a
// 'You:' prefix; assistant lines get none. A line whose text starts with
// '[tool:' is dimmed regardless of role (it's a tool-call marker, not prose).
function peekLineModel(line) {
  const text = String((line && line.text) || '');
  const role = line && line.role;
  const isTool = /^\s*\[tool:/i.test(text);
  return {
    prefix: role === 'user' ? 'You:' : '',
    text,
    dim: isTool,
  };
}

// Builds the toast summary for an /api/upload response. Saved count (singular/
// plural), plus a rejected note: a single rejection names its reason, multiple
// just count. Empty both -> 'nothing uploaded'.
function uploadSummaryText(res) {
  const saved = (res && res.saved) || [];
  const rejected = (res && res.rejected) || [];
  const parts = [];
  if (saved.length) parts.push(`${saved.length} uploaded`);
  if (rejected.length === 1) {
    parts.push(`1 rejected: ${rejected[0].reason}`);
  } else if (rejected.length > 1) {
    parts.push(`${rejected.length} rejected`);
  }
  if (!parts.length) return 'nothing uploaded';
  return parts.join(' · ');
}

// Picks the success toast for an /api/spawn response {mode, firstTime}. A
// first-time or visible spawn opens a real terminal on the PC (trust prompt on
// first run); a hidden spawn runs in the background, reachable via the Claude
// app's Code tab.
function spawnToastText(res) {
  const mode = res && res.mode;
  const firstTime = res && res.firstTime;
  if (mode === 'visible' || firstTime) {
    return 'opened a terminal on the PC — answer the one-time trust prompt there';
  }
  return 'started in background — open the Code tab in your Claude app';
}

// Maps an /api/search result into a display model (adds a folder/file icon).
function searchResultDisplay(r) {
  return {
    name: r.name,
    parent: r.parent,
    isDir: !!r.isDir,
    path: r.path,
    icon: r.isDir ? '📁' : '📄',
  };
}

// Two-tap inline confirm reducer (used by the destructive Kill buttons instead
// of a confirm overlay). state shape: { armed: bool, armedAt: number }.
// A 'tap' while disarmed arms it (action 'arm'); a 'tap' while armed AND still
// within `windowMs` of arming fires (action 'confirm'); a 'tap' after the
// window re-arms fresh. A 'timeout' event disarms (action 'revert') only if the
// arming window has actually elapsed -- a stale timer from a prior arm is
// ignored. Pure: no timers, caller drives 'tap'/'timeout' with a clock.
function twoTapReduce(state, event, now, windowMs) {
  if (event === 'tap') {
    if (state.armed && (now - state.armedAt) < windowMs) {
      return { state: { armed: false, armedAt: 0 }, action: 'confirm' };
    }
    return { state: { armed: true, armedAt: now }, action: 'arm' };
  }
  if (event === 'timeout') {
    if (state.armed && (now - state.armedAt) >= windowMs) {
      return { state: { armed: false, armedAt: 0 }, action: 'revert' };
    }
    return { state, action: null };
  }
  return { state, action: null };
}
// --- end files-ui-pure ---

// ---- Reconnect backoff (shared by Bridge audio/mic sockets and PTT) --------
// Bounded exponential-ish backoff: same schedule everywhere so a dropped
// socket doesn't hammer the server, but also doesn't take forever to notice
// the network is back. Each caller gets its own independent scheduler
// instance (own attempt counter + timer) since Bridge audio, Bridge mic, and
// PTT can all be reconnecting on unrelated schedules at once.
const WS_BACKOFF_MS = [250, 500, 1000, 2000, 4000];

function createBackoffScheduler(reconnectFn, shouldReconnectFn) {
  let attempts = 0;
  let timer = null;
  return {
    schedule() {
      if (timer) return; // already pending
      if (!shouldReconnectFn()) return;
      const delay = WS_BACKOFF_MS[Math.min(attempts, WS_BACKOFF_MS.length - 1)];
      attempts += 1;
      timer = setTimeout(() => {
        timer = null;
        if (!shouldReconnectFn()) return;
        reconnectFn();
      }, delay);
    },
    reset() {
      attempts = 0;
      if (timer) { clearTimeout(timer); timer = null; }
    },
    cancel() {
      if (timer) { clearTimeout(timer); timer = null; }
    },
    get pending() { return timer !== null; },
  };
}

// Audio-out state.
let audioCtx = null;
let masterGain = null;
let workletNode = null;
let ws = null;
let format = { sampleRate: 48000, channels: 2 };
let bytesIn = 0;
let lastStatsAt = 0;
let lastAudioCloseAt = 0; // For "reconnected" toast threshold.
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

// User intent: true from the moment they tap "start" until they tap "stop"
// (or Bridge mode is exited). Reconnect only happens while this is true --
// an unexpected drop auto-heals, but a deliberate stop must stay stopped.
let audioIntentOn = false;
const audioWsBackoff = createBackoffScheduler(() => openAudioWs(), () => audioIntentOn);

async function startAudio() {
  audioIntentOn = true;
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
    audioIntentOn = false;
    setAudioState(false, 'audio init failed');
    setFooter(friendlyError(e));
    return;
  }

  openAudioWs();
}

// Opens/reopens just the /audio socket. Split out from startAudio() so a
// reconnect can re-use the already-warm AudioContext/worklet instead of
// re-running the whole init sequence (and re-prompting anything iOS would
// otherwise gate behind a fresh user gesture).
function openAudioWs() {
  if (ws && ws.readyState !== WebSocket.CLOSED) return;
  audioWsBackoff.cancel();
  setAudioState(false, lastAudioCloseAt ? 'reconnecting…' : 'connecting…');

  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  ws = new WebSocket(`${proto}//${location.host}/audio`);
  ws.binaryType = 'arraybuffer';

  ws.onopen = () => {
    audioWsBackoff.reset();
    bytesIn = 0;
    lastStatsAt = performance.now();
    setAudioState(true, 'streaming');
    if (lastAudioCloseAt && (Date.now() - lastAudioCloseAt) > 1000) {
      toast('audio out reconnected');
    }
    lastAudioCloseAt = 0;
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
    ws = null;
    lastAudioCloseAt = Date.now();
    if (audioIntentOn) {
      // Unexpected drop while the user still wants this on -- keep the
      // AudioContext/worklet alive and just reconnect the transport, same
      // pattern as the PTT socket.
      setAudioState(false, 'reconnecting…');
      audioWsBackoff.schedule();
    } else {
      setAudioState(false, 'disconnected');
      teardownAudio();
    }
  };

  ws.onerror = () => {
    // Silent, like the PTT socket -- onclose is what drives recovery, and iOS
    // fires error on routine backgrounding too.
  };
}

function teardownAudio() {
  audioWsBackoff.cancel();
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
  audioIntentOn = false;
  audioWsBackoff.cancel();
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

// Same intent-gated reconnect pattern as Audio out (see audioIntentOn above).
let micIntentOn = false;
let lastMicCloseAt = 0; // For "reconnected" toast threshold.
const micWsBackoff = createBackoffScheduler(() => openMicWs(), () => micIntentOn);

async function startMic() {
  micIntentOn = true;
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
    micIntentOn = false;
    setMicState(false, 'mic denied');
    setFooter(friendlyError(e));
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
    // Pump captured PCM out as soon as the worklet hands it to us. Reads
    // micWs live (module-level var) so it keeps working across reconnects
    // without needing to be re-bound.
    micCaptureNode.port.onmessage = (e) => {
      const d = e.data;
      if (d && d.type === 'pcm' && micWs && micWs.readyState === WebSocket.OPEN) {
        micWs.send(d.samples.buffer);
        micBytesOut += d.samples.byteLength;
      }
    };
  } catch (e) {
    micIntentOn = false;
    setMicState(false, 'audio init failed');
    setFooter(friendlyError(e));
    teardownMic();
    return;
  }

  openMicWs();
}

// Opens/reopens just the /mic socket. Split out from startMic() so a
// reconnect can re-use the already-warm mic stream/worklet -- iOS keeps the
// mic permission and MediaStream alive across a socket drop, only the
// transport died.
function openMicWs() {
  if (micWs && micWs.readyState !== WebSocket.CLOSED) return;
  micWsBackoff.cancel();
  setMicState(false, lastMicCloseAt ? 'reconnecting…' : 'connecting…');

  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  micWs = new WebSocket(`${proto}//${location.host}/mic`);
  micWs.binaryType = 'arraybuffer';

  micWs.onopen = () => {
    micWsBackoff.reset();
    micBytesOut = 0;
    setMicState(true, 'live');
    if (lastMicCloseAt && (Date.now() - lastMicCloseAt) > 1000) {
      toast('mic in reconnected');
    }
    lastMicCloseAt = 0;
  };

  micWs.onclose = () => {
    micWs = null;
    lastMicCloseAt = Date.now();
    if (micIntentOn) {
      // Unexpected drop while the user still wants this on -- keep the mic
      // stream/worklet alive and just reconnect the transport.
      setMicState(false, 'reconnecting…');
      micWsBackoff.schedule();
    } else {
      setMicState(false, 'disconnected');
      teardownMic();
    }
  };
  micWs.onerror = () => {
    // Silent, like the PTT socket -- onclose drives recovery.
  };
}

function teardownMic() {
  micWsBackoff.cancel();
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
  micIntentOn = false;
  micWsBackoff.cancel();
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
// The server presses the dictation app's hotkey (config `ptt_hotkey`, default
// Wispr Flow's hands-free toggle) immediately on start, and on stop it waits
// for the WASAPI render queue + VB-CABLE to drain before pressing it again --
// so the app sees the full tail of the user's speech before it stops listening.

const pttBtn = document.getElementById('ptt-btn');
const pttLabelEl = document.getElementById('ptt-label');
const pttStateEl = document.getElementById('ptt-state');

let pttActivated = false;
let pttTransmitting = false;
let pttCtx = null;
let pttStream = null;
let pttCaptureNode = null;
let pttWs = null;
let pttDrainTimer = null;

// Which server behaviour the big button drives. PTT mode routes mic audio to
// the virtual cable and presses the hotkey so Wispr Flow hears it; Dictate mode has
// the bridge keep the audio and run whisper.cpp on it. Everything else --
// socket, reconnect, press handling, mic graph -- is identical, so the two
// modes differ only in this prefix.
let talkProtocol = 'ptt';

// Lifecycle / reconnect state. iOS suspends WebSockets, AudioContexts, and
// MediaStream tracks when the home-screen web clip backgrounds, so the socket
// disappears constantly. We treat pttWs as ephemeral and auto-heal it.
let pttModeActive = false;             // True only while view-ptt is showing.
let pttWsLastCloseAt = 0;              // For "reconnected" toast threshold.
const pendingKeys = [];                // { name, expiresAt } -- discrete key presses queued during outage.
const PENDING_KEY_TTL_MS = 1500;       // Long enough to ride a typical iOS resume, short enough that stale Esc doesn't fire late.
const PENDING_KEY_CAP = 16;            // Drop oldest if a long outage piles things up.
const pttWsBackoff = createBackoffScheduler(() => openPttWs(), () => pttModeActive);
// True when the user pressed PTT but the WS wasn't OPEN yet, so we haven't
// actually sent ptt:start. Without this the first press after Slide-Over
// returns is eaten by the reconnect window: the start is dropped silently,
// release sends a stop, server presses the hotkey anyway, and the dictation
// app interprets the lone press as "start listening" -- forcing a second tap
// to toggle off.
let pttStartPending = false;

const dictateBtn = document.getElementById('dictate-btn');
const dictateLabelEl = document.getElementById('dictate-label');
const dictateStateEl = document.getElementById('dictate-state');

// Both modes drive the same reducer and socket, so drive both button faces
// too. Only one view is ever visible, so writing to both is free.
function setPttUi(label, stateMsg, cls /* 'on' | 'draining' | null */) {
  const inDictate = talkProtocol === 'dictate';
  const labelEl = inDictate ? dictateLabelEl : pttLabelEl;
  const stateEl = inDictate ? dictateStateEl : pttStateEl;
  const btn = inDictate ? dictateBtn : pttBtn;
  // "Push to Talk" is PTT's vocabulary; dictation says what it's doing.
  if (inDictate && label === 'Push to Talk') label = 'Dictate';
  labelEl.textContent = label;
  stateEl.textContent = stateMsg;
  btn.classList.toggle('on', cls === 'on');
  btn.classList.toggle('draining', cls === 'draining');
}

// --- dictate-ui-pure (pure; tested by tests/dictate-ui.test.mjs) ---

// How long a recording was, phrased for a status line.
function dictateDuration(seconds) {
  if (typeof seconds !== 'number' || !isFinite(seconds) || seconds <= 0) {
    return 'no audio captured';
  }
  if (seconds < 60) return seconds.toFixed(1) + 's of audio';
  const mins = Math.floor(seconds / 60);
  const rem = Math.round(seconds % 60);
  return rem === 0 ? mins + 'm of audio' : mins + 'm ' + rem + 's of audio';
}

// Map a server dictation frame onto what the button should show. Returns null
// for frames this view doesn't own, so the caller can ignore them.
function dictateView(msg) {
  if (!msg || msg.type !== 'dictation') return null;
  switch (msg.state) {
    case 'recording':
      return { label: 'Listening…', state: 'tap to stop', cls: 'on', text: null };
    case 'transcribing':
      return { label: 'Transcribing…', state: dictateDuration(msg.seconds), cls: 'draining', text: null };
    case 'done': {
      const text = typeof msg.text === 'string' ? msg.text : '';
      const state = msg.overflowed
        ? 'hit the 10 minute limit'
        : (text ? 'typed into your PC' : 'nothing heard');
      return { label: 'Dictate', state, cls: null, text };
    }
    case 'error':
      return { label: 'Dictate', state: msg.error || 'transcription failed', cls: null, text: null };
    default:
      return null;
  }
}

// --- end dictate-ui-pure ---

// Open the keys/control WebSocket. Doesn't require any iOS permission, so we
// can do this the moment the user enters PTT mode (or restores it on load).
// Nav-key buttons just need this open to work; only PTT itself needs the mic.
// The socket is treated as ephemeral: iOS will close it on every backgrounding,
// and we auto-reconnect with bounded backoff. Mic context/stream are kept
// alive across socket drops -- only the transport died, not the permission.
function openPttWs() {
  if (pttWs && pttWs.readyState !== WebSocket.CLOSED) return;
  pttWsBackoff.cancel();
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  pttWs = new WebSocket(`${proto}//${location.host}/mic`);
  pttWs.binaryType = 'arraybuffer';

  pttWs.onopen = () => {
    pttWsBackoff.reset();
    // Only toast for outages long enough to actually notice -- otherwise every
    // quick iOS focus-blur flashes a confirmation, which is noisy.
    if (pttWsLastCloseAt && (Date.now() - pttWsLastCloseAt) > 1000) {
      toast('reconnected');
    }
    pttWsLastCloseAt = 0;
    flushPendingKeys();
    // If the user pressed PTT during the reconnect window, the start was
    // deferred. Fire it now -- but only if they're still pressing.
    if (pttStartPending && pttTransmitting) {
      try { pttWs.send(talkProtocol + ':start'); } catch (_) {}
    }
    pttStartPending = false;
    // Drop the "reconnecting…" message if that's what's showing.
    if (pttActivated && pttStateEl.textContent === 'reconnecting…') {
      setPttUi('Push to Talk', pttTransmitting ? 'release / tap to stop' : 'tap or hold',
               pttTransmitting ? 'on' : null);
    }
  };

  pttWs.onmessage = (e) => {
    if (typeof e.data !== 'string') return;
    let msg;
    try { msg = JSON.parse(e.data); } catch (_) { return; }
    const view = dictateView(msg);
    if (!view) return;
    setPttUi(view.label, view.state, view.cls);
    if (msg.state === 'done') {
      // Carries mode/warning as well as text, so it owns the result card.
      const done = showDictateDone(msg);
      // A failed AI step is not a failed dictation -- say what actually
      // happened, but only when there IS a warning to explain.
      if (done.notice) setPttUi(view.label, done.state, view.cls);
    } else if (view.text !== null) {
      showDictateResult(view.text);
    }
  };

  pttWs.onclose = () => {
    pttWsLastCloseAt = Date.now();
    pttWs = null;
    // The socket dropped, but the mic context/stream and permission are still
    // ours. Don't tear them down -- just reconnect.
    pttTransmitting = false;
    if (pttModeActive) {
      pttWsBackoff.schedule();
      if (pttActivated) {
        setPttUi('Push to Talk', 'reconnecting…', null);
      }
    }
  };

  pttWs.onerror = () => {
    // Silent: iOS fires this on every backgrounding cycle. The onclose handler
    // is what actually drives recovery.
  };
}

function flushPendingKeys() {
  const now = Date.now();
  while (pendingKeys.length) {
    const k = pendingKeys.shift();
    if (k.expiresAt < now) continue;
    if (pttWs && pttWs.readyState === WebSocket.OPEN) {
      pttWs.send('key:' + k.name);
    } else {
      // Socket died again mid-flush; put it back and stop.
      pendingKeys.unshift(k);
      break;
    }
  }
}

// Generation token: increments on every press AND release. Any async work
// that started under a given gen aborts if pttTalkGen has moved on -- prevents
// a slow getUserMedia from leaving a hot mic after the user already released.
let pttTalkGen = 0;

// "Activate" only PRIMES the mic permission. We acquire getUserMedia briefly
// to satisfy the iOS permission prompt, then release the tracks immediately
// so iOS deactivates the voice-chat audio session -- otherwise other apps
// (Moonlight desktop audio on a split-screen iPad, etc.) get ducked or muted
// the entire time PTT mode is open, even when not actively talking.
// The real mic acquisition happens on each PTT press in startTransmitting().
async function activatePtt() {
  setPttUi('…', 'asking permission…', null);
  let primer;
  try {
    primer = await navigator.mediaDevices.getUserMedia({
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
    setFooter(friendlyError(e));
    return;
  }
  // Release immediately -- iOS will deactivate the voice-chat session within
  // a few hundred ms and other apps get their audio back.
  primer.getTracks().forEach((t) => t.stop());

  openPttWs();
  pttActivated = true;
  setPttUi('Push to Talk', 'tap or hold', null);
}

// Tear down the per-transmission mic graph. Idempotent. Does NOT reset
// pttActivated -- the user still has permission, we just released the
// audio session.
function teardownTalkingMic() {
  if (pttCaptureNode) {
    try { pttCaptureNode.port.onmessage = null; } catch (_) {}
    try { pttCaptureNode.disconnect(); } catch (_) {}
    pttCaptureNode = null;
  }
  if (pttCtx) { pttCtx.close().catch(() => {}); pttCtx = null; }
  if (pttStream) {
    // Unwire the track listeners BEFORE stopping -- otherwise calling .stop()
    // ourselves fires track.onended, which would tear down activation state
    // as if iOS had revoked permission. Only genuine system-side stops (mic
    // permission revoked, page killed) should reach the onended handler.
    pttStream.getTracks().forEach((t) => {
      t.onended = null;
      t.onmute = null;
      t.stop();
    });
    pttStream = null;
  }
}

// Full PTT teardown -- only used on mode exit.
function teardownPtt() {
  if (pttDrainTimer) { clearTimeout(pttDrainTimer); pttDrainTimer = null; }
  pttTalkGen++; // invalidate any in-flight startTransmitting
  pttTransmitting = false;
  pttActivated = false;
  pttStartPending = false;
  teardownTalkingMic();
  if (pttWs) {
    try { pttWs.close(); } catch (_) {}
    pttWs = null;
  }
}

// Press starts here. Press the hotkey server-side FIRST (before the
// ~100-300ms iOS mic spinup) so the dictation app starts listening as fast
// as possible. PCM frames begin flowing once the AudioContext is ready; the
// very first transient of speech may be lost but everything from "spinup
// done" onward is captured, and the app's own VAD handles the leading edge.
async function startTransmitting() {
  pttTransmitting = true;
  setPttUi('Listening…', 'release / tap to stop', 'on');

  if (pttWs && pttWs.readyState === WebSocket.OPEN) {
    pttWs.send(talkProtocol + ':start');
    pttStartPending = false;
  } else {
    // WS is reconnecting (typically after Slide-Over return on iPad). Defer
    // the start until onopen fires; if the user releases first, stopTransmitting
    // will clear the flag.
    pttStartPending = true;
    if (pttModeActive && !pttWsBackoff.pending && (!pttWs || pttWs.readyState === WebSocket.CLOSED)) {
      openPttWs();
    }
  }

  const myGen = ++pttTalkGen;

  let stream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({
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
    if (myGen !== pttTalkGen) return;
    pttTransmitting = false;
    pttActivated = false;
    setPttUi('Activate', 'mic acquire failed', null);
    setFooter(friendlyError(e));
    if (pttWs && pttWs.readyState === WebSocket.OPEN) {
      pttWs.send(talkProtocol + ':stop');
    }
    return;
  }

  // User released (or mode changed) while getUserMedia was in flight -- drop.
  if (myGen !== pttTalkGen) {
    stream.getTracks().forEach((t) => t.stop());
    return;
  }
  pttStream = stream;

  // Genuine permission loss / system kill during transmission.
  const track = pttStream.getAudioTracks()[0];
  if (track) {
    track.onended = () => {
      pttTalkGen++;
      pttTransmitting = false;
      pttActivated = false;
      teardownTalkingMic();
      setPttUi('Activate', 'mic stream ended', null);
      if (pttWs && pttWs.readyState === WebSocket.OPEN) {
        try { pttWs.send(talkProtocol + ':stop'); } catch (_) {}
      }
    };
    track.onmute = () => { toast('mic muted by system'); };
  }

  try {
    pttCtx = new (window.AudioContext || window.webkitAudioContext)({
      latencyHint: 'interactive',
      sampleRate: 48000,
    });
    await pttCtx.resume();
    await pttCtx.audioWorklet.addModule('/mic-worklet.js');
  } catch (e) {
    if (myGen !== pttTalkGen) { teardownTalkingMic(); return; }
    pttTransmitting = false;
    setPttUi('Push to Talk', 'audio init failed', null);
    setFooter(friendlyError(e));
    teardownTalkingMic();
    if (pttWs && pttWs.readyState === WebSocket.OPEN) {
      pttWs.send(talkProtocol + ':stop');
    }
    return;
  }

  // Second race check -- worklet load can take 10-50ms.
  if (myGen !== pttTalkGen) { teardownTalkingMic(); return; }

  const source = pttCtx.createMediaStreamSource(pttStream);
  pttCaptureNode = new AudioWorkletNode(pttCtx, 'mic-capture', {
    numberOfInputs: 1,
    numberOfOutputs: 0,
  });
  source.connect(pttCaptureNode);

  pttCaptureNode.port.onmessage = (e) => {
    if (!pttTransmitting) return;
    const d = e.data;
    if (d && d.type === 'pcm' && pttWs && pttWs.readyState === WebSocket.OPEN) {
      pttWs.send(d.samples.buffer);
    }
  };
}

function stopTransmitting() {
  pttTalkGen++; // invalidate any in-flight startTransmitting
  pttTransmitting = false;
  // If the start was queued but never sent (WS was reconnecting), drop it on
  // the floor -- don't send a lone ptt:stop, which would tap Alt server-side
  // and put SuperWhisper into a hot state the user never asked for.
  if (pttStartPending) {
    pttStartPending = false;
  } else if (pttWs && pttWs.readyState === WebSocket.OPEN) {
    pttWs.send(talkProtocol + ':stop');
  }
  // Tear the mic + audio session down right away -- iOS releases the voice
  // session within a few hundred ms and other apps get their audio back.
  // PCM frames already on the wire are ahead of the ptt:stop frame, and TCP
  // ordering guarantees the server processes them before tapping Alt.
  teardownTalkingMic();
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
// still cleanly closes the transmission instead of leaving the dictation app hot.
//
// HOLD_MS was originally 250, which misread relaxed thumb taps (250-400ms of
// contact) as holds: the transmission opened on touch and closed on lift,
// producing instant start/stop "glitches" and 0.0s dictations in bridge.log.
// 600ms means only a deliberate press-and-speak counts as a hold.
const PTT_HOLD_MS = 600;

// --- ptt-press-machine (pure; tested by tests/ptt-press.test.mjs) ---
// No DOM/globals in here -- event handlers below are thin adapters that feed
// this reducer 'down'/'up'/'cancel' events and execute the actions it returns.
//
// state shape: {
//   activated: bool,        // has the user completed Activate at least once
//   transmitting: bool,     // is a transmission currently open
//   pressOpenedTx: bool,    // did the CURRENT press open the transmission (vs. it
//                           // already being open when this press landed)
//   pressTime: number,      // Date.now() at the start of the current press
//   activePointerId: any,   // pointerId of the in-flight press, or null if none
// }
//
// A "duplicate" pointerdown (iOS/iPadOS synthesizing an extra pointerdown for
// the same physical touch) is identified STRUCTURALLY: it arrives while a
// press is already active (no pointerup/pointercancel seen yet in between),
// not by a wall-clock guess. A real second tap always has an up/cancel
// between the two downs, so it's never mistaken for a duplicate.
function pttPressReduce(state, event, now) {
  const actions = [];
  let next = state;

  if (event.type === 'down') {
    // A duplicate synthetic pointerdown carries the SAME pointerId as the
    // press already in flight -- that's what distinguishes it from a real
    // second tap. Ignore only those.
    //
    // A *different* pointerId means the previous press never delivered its
    // up/cancel: iOS drops them whenever it steals a gesture (control centre,
    // call banner, Slide-Over). Adopting the new press is essential -- holding
    // the stale id would discard every future press as a duplicate and leave
    // the button permanently dead until reload.
    if (state.activePointerId === event.pointerId
        && state.activePointerId !== null && state.activePointerId !== undefined) {
      return { state, actions };
    }
    next = { ...state, activePointerId: event.pointerId };

    if (!state.activated) {
      actions.push('activate');
      return { state: next, actions };
    }

    if (!state.transmitting) {
      next = { ...next, transmitting: true, pressOpenedTx: true, pressTime: now };
      actions.push('startTx');
    } else {
      next = { ...next, transmitting: false, pressOpenedTx: false };
      actions.push('stopTx');
    }
    return { state: next, actions };
  }

  if (event.type === 'up' || event.type === 'cancel') {
    // Only the pointer that owns the active press can end it.
    if (state.activePointerId !== event.pointerId) {
      return { state, actions };
    }
    next = { ...state, activePointerId: null };

    if (!state.activated || !state.transmitting || !state.pressOpenedTx) {
      return { state: next, actions };
    }

    const held = now - state.pressTime;
    next = { ...next, pressOpenedTx: false };
    if (held >= PTT_HOLD_MS) {
      // Real hold -> release stops transmission.
      next = { ...next, transmitting: false };
      actions.push('stopTx');
    }
    // Otherwise the user just tapped quickly; leave the transmission live so
    // the next tap toggles it off.
    return { state: next, actions };
  }

  return { state, actions };
}
// --- end ptt-press-machine ---

let pttPressState = {
  activated: false,
  transmitting: false,
  pressOpenedTx: false,
  pressTime: 0,
  activePointerId: null,
};

function runPttAction(action) {
  if (action === 'activate') activatePtt();
  else if (action === 'startTx') startTransmitting();
  else if (action === 'stopTx') stopTransmitting();
}

function pttPressDown(e) {
  if (e.cancelable) e.preventDefault();
  try { e.currentTarget.setPointerCapture(e.pointerId); } catch (_) {}
  // Keep the reducer's view of "activated" in sync with the real world --
  // activation can complete/fail asynchronously between presses.
  pttPressState = { ...pttPressState, activated: pttActivated, transmitting: pttTransmitting };
  const { state, actions } = pttPressReduce(pttPressState, { type: 'down', pointerId: e.pointerId }, Date.now());
  pttPressState = state;
  actions.forEach(runPttAction);
}

function pttPressUp(e) {
  pttPressState = { ...pttPressState, activated: pttActivated, transmitting: pttTransmitting };
  const { state, actions } = pttPressReduce(pttPressState, { type: 'up', pointerId: e.pointerId }, Date.now());
  pttPressState = state;
  actions.forEach(runPttAction);
}

function pttPressCancel(e) {
  pttPressState = { ...pttPressState, activated: pttActivated, transmitting: pttTransmitting };
  const { state, actions } = pttPressReduce(pttPressState, { type: 'cancel', pointerId: e.pointerId }, Date.now());
  pttPressState = state;
  actions.forEach(runPttAction);
}

pttBtn.addEventListener('pointerdown', pttPressDown);
pttBtn.addEventListener('pointerup', pttPressUp);
pttBtn.addEventListener('pointercancel', pttPressCancel);

// Dictate's button shares the reducer -- only one of the two views is ever
// visible, so there's no ambiguity about which press is being handled.
dictateBtn.addEventListener('pointerdown', pttPressDown);
dictateBtn.addEventListener('pointerup', pttPressUp);
dictateBtn.addEventListener('pointercancel', pttPressCancel);

const dictateResultEl = document.getElementById('dictate-result');
const dictateTextEl = document.getElementById('dictate-text');

function showDictateResult(text) {
  if (!text) { dictateResultEl.hidden = true; return; }
  dictateTextEl.textContent = text;
  dictateResultEl.hidden = false;
}

document.getElementById('dictate-copy').addEventListener('click', () => {
  navigator.clipboard.writeText(dictateTextEl.textContent || '')
    .then(() => toast('copied'))
    .catch(() => toast('copy failed'));
});

// The address the Shortcut needs. Derived from wherever the page is being
// served, so it's right whether that's the Tailscale name or a LAN address.
function bindCopyableUrl(textId, buttonId, url) {
  document.getElementById(textId).textContent = url;
  document.getElementById(buttonId).addEventListener('click', () => {
    navigator.clipboard.writeText(url)
      .then(() => toast('address copied'))
      .catch(() => toast('copy failed'));
  });
}

// Send the dictated line without switching to PTT just to press Enter.
document.getElementById('dictate-enter').addEventListener('click', () => sendKey('enter'));

bindCopyableUrl('dictate-api-url', 'dictate-api-copy', location.origin + '/api/dictate');
bindCopyableUrl('dictate-url', 'dictate-url-copy', location.origin + '/#dictate');

// ---- Nav controls (arrows + editing + tab/desktop switching) -------------
// All key commands ride on the same /mic WebSocket as text frames. They are
// no-ops until the user has tapped Activate (since that's when pttWs opens).

// Discrete one-shot key presses (Esc, Enter, /btw, etc.). If the WS is open,
// fire immediately. Otherwise queue with a short TTL so a tap during a brief
// reconnect lands once the socket is back -- past the TTL, the press is
// dropped so a stale Esc doesn't fire long after the user moved on.
function sendKey(name) {
  if (pttWs && pttWs.readyState === WebSocket.OPEN) {
    pttWs.send('key:' + name);
    return;
  }
  pendingKeys.push({ name, expiresAt: Date.now() + PENDING_KEY_TTL_MS });
  if (pendingKeys.length > PENDING_KEY_CAP) pendingKeys.shift();
  // Make sure a reconnect attempt is in flight so the queue can drain.
  if (pttModeActive && !pttWsBackoff.pending && (!pttWs || pttWs.readyState === WebSocket.CLOSED)) {
    openPttWs();
  }
}

// Variant for hold-to-repeat keys (arrows, backspace, space, ctrl-w). Queueing
// these would be a disaster: a 2-second outage during a long backspace hold
// would replay 40 stale deletes on reconnect. The repeater itself is the
// retry mechanism -- just drop if the socket is down.
function sendKeyNoQueue(name) {
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

// Lock the page: kill iOS Safari's rubber-band drag entirely. CSS
// touch-action: none on body covers most cases, but a non-passive touchmove
// preventDefault is the belt-and-braces guarantee that no finger drag ever
// scrolls or overscrolls the layout — EXCEPT inside the Files tab's real
// scroll containers, which must keep native scrolling.
document.addEventListener('touchmove', (e) => {
  if (e.target instanceof Element &&
      e.target.closest('.files-scroll, .chip-row, .breadcrumb-bar, .peek-body, .viewer-body, .git-list')) return;
  if (e.cancelable) e.preventDefault();
}, { passive: false });

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
    sendKeyNoQueue(name);
    delayTimer = setTimeout(() => {
      delayTimer = null;
      repeatTimer = setInterval(() => sendKeyNoQueue(name), HOLD_INTERVAL_MS);
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
document.getElementById('key-task-view').addEventListener('click', () => sendKey('task-view'));
document.getElementById('key-alt-tab').addEventListener('click', () => sendKey('alt-tab'));
document.getElementById('key-ctrl-tab').addEventListener('click', () => sendKey('ctrl-tab'));
document.getElementById('key-shift-tab').addEventListener('click', () => sendKey('shift-tab'));
document.getElementById('key-ctrl-u').addEventListener('click', () => sendKey('ctrl-u'));
document.getElementById('key-ctrl-k').addEventListener('click', () => sendKey('ctrl-k'));
document.getElementById('key-ctrl-c').addEventListener('click', () => sendKey('ctrl-c'));
document.getElementById('key-ctrl-enter').addEventListener('click', () => sendKey('ctrl-enter'));
document.getElementById('key-btw').addEventListener('click', () => sendKey('btw'));
document.getElementById('key-resume').addEventListener('click', () => sendKey('resume'));
document.getElementById('key-push').addEventListener('click', () => sendKey('push'));
document.getElementById('key-min-window').addEventListener('click', () => sendKey('min-window'));
document.getElementById('key-clear').addEventListener('click', () => sendKey('clear'));

// Dictate-view fix-up keys: Select All + Backspace let a bad dictation be
// wiped and redone from the phone. Same /mic transport as the PTT nav keys,
// which is open in Dictate mode too (TALK_MODES share the socket).
document.getElementById('dictate-ctrl-a').addEventListener('click', () => sendKey('ctrl-a'));
bindHoldRepeat(document.getElementById('dictate-backspace'), 'backspace');

// ---- Files tab --------------------------------------------------------
// Browse PC folders, manage them, and spawn Claude Code sessions. All network
// access goes through apiFetch() so PIN injection / 401-overlay / error-toast
// live in one place, per the server contract in the project brief.

const PIN_KEY = 'files.pin';
const FAVORITES_KEY = 'files.favorites';
const RECENTS_KEY = 'files.recents';
const LAST_LOCATION_KEY = 'files.lastLocation';

const pinOverlay = document.getElementById('pin-overlay');
const pinPad = document.getElementById('pin-pad');
const pinDots = document.getElementById('pin-dots');
const pinSub = document.getElementById('pin-sub');

const promptOverlay = document.getElementById('prompt-overlay');
const promptTitle = document.getElementById('prompt-title');
const promptInput = document.getElementById('prompt-input');
const promptCancelBtn = document.getElementById('prompt-cancel-btn');
const promptOkBtn = document.getElementById('prompt-ok-btn');

const confirmOverlay = document.getElementById('confirm-overlay');
const confirmTitle = document.getElementById('confirm-title');
const confirmCancelBtn = document.getElementById('confirm-cancel-btn');
const confirmOkBtn = document.getElementById('confirm-ok-btn');

const sheetOverlay = document.getElementById('sheet-overlay');
const sheetTitle = document.getElementById('sheet-title');
const sheetNewFolderBtn = document.getElementById('sheet-new-folder');
const sheetRenameBtn = document.getElementById('sheet-rename');
const sheetFavoriteBtn = document.getElementById('sheet-favorite');
const sheetDeleteBtn = document.getElementById('sheet-delete');
const sheetCancelBtn = document.getElementById('sheet-cancel');

const spawnOverlay = document.getElementById('spawn-overlay');
const spawnSheetTitle = document.getElementById('spawn-sheet-title');
const spawnVisibleBtn = document.getElementById('spawn-visible-btn');
const spawnHiddenBtn = document.getElementById('spawn-hidden-btn');
const spawnCustomBtn = document.getElementById('spawn-custom-btn');
const spawnCancelBtn = document.getElementById('spawn-cancel-btn');

const sessionsListEl = document.getElementById('sessions-list');
const chipsShortcutsEl = document.getElementById('chips-shortcuts');
const recentHeadEl = document.getElementById('recent-head');
const recentSeeAllBtn = document.getElementById('recent-see-all-btn');
const recentListEl = document.getElementById('recent-list');
const breadcrumbRowEl = document.getElementById('breadcrumb-row');
const breadcrumbBarEl = document.getElementById('breadcrumb-bar');
const currentFavBtn = document.getElementById('current-fav-btn');
const filesNewFolderBtn = document.getElementById('files-new-folder-btn');
const filesStartHereBtn = document.getElementById('files-start-here-btn');
const filesHeaderRowEl = document.getElementById('files-header-row');
const filesUploadBtn = document.getElementById('files-upload-btn');
const filesUploadInput = document.getElementById('files-upload-input');
const filesSortBtn = document.getElementById('files-sort-btn');
const filesSearchRowEl = document.getElementById('files-search-row');
const filesSearchInput = document.getElementById('files-search-input');
const filesSearchClearBtn = document.getElementById('files-search-clear');
const filesSearchAllBtn = document.getElementById('files-search-all-btn');
const entryListEl = document.getElementById('entry-list');
const searchResultsEl = document.getElementById('search-results');
const trashBreadcrumbRowEl = document.getElementById('trash-breadcrumb-row');
const trashBackBtn = document.getElementById('trash-back-btn');
const trashListEl = document.getElementById('trash-list');
const historyBreadcrumbRowEl = document.getElementById('history-breadcrumb-row');
const historyBackBtn = document.getElementById('history-back-btn');
const historyListEl = document.getElementById('history-list');
const ptrHintEl = document.getElementById('ptr-hint');
const filesScrollEl = document.getElementById('files-scroll');

const gitOverlay = document.getElementById('git-overlay');
const gitTitle = document.getElementById('git-title');
const gitListEl = document.getElementById('git-list');
const gitCloseBtn = document.getElementById('git-close-btn');

const peekOverlay = document.getElementById('peek-overlay');
const peekTitle = document.getElementById('peek-title');
const peekBodyEl = document.getElementById('peek-body');
const peekCloseBtn = document.getElementById('peek-close-btn');
const peekKillBtn = document.getElementById('peek-kill-btn');

const viewerOverlay = document.getElementById('viewer-overlay');
const viewerTitle = document.getElementById('viewer-title');
const viewerSub = document.getElementById('viewer-sub');
const viewerBodyEl = document.getElementById('viewer-body');
const viewerCloseBtn = document.getElementById('viewer-close-btn');
const viewerDownloadBtn = document.getElementById('viewer-download-btn');

const FILES_SORT_KEY = 'files.sort';
function getFilesSort() { const v = lsGet(FILES_SORT_KEY, 'name'); return v === 'modified' ? 'modified' : 'name'; }
function setFilesSort(v) { lsSet(FILES_SORT_KEY, v === 'modified' ? 'modified' : 'name'); }

// ---- tiny localStorage helpers (all guarded -- iOS private mode throws) --

function lsGet(key, fallback) {
  try {
    const raw = localStorage.getItem(key);
    if (raw === null) return fallback;
    return JSON.parse(raw);
  } catch (_) {
    return fallback;
  }
}
function lsSet(key, value) {
  try { localStorage.setItem(key, JSON.stringify(value)); } catch (_) { /* private mode */ }
}

// Builds an <svg class="ic"><use href="#ic-*"></svg> referencing the sprite in
// index.html. Single source of truth for every dynamically-rendered glyph.
const SVG_NS = 'http://www.w3.org/2000/svg';
const XLINK_NS = 'http://www.w3.org/1999/xlink';
function svgIcon(id, extraClass) {
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('class', 'ic' + (extraClass ? ' ' + extraClass : ''));
  svg.setAttribute('aria-hidden', 'true');
  const use = document.createElementNS(SVG_NS, 'use');
  use.setAttribute('href', '#ic-' + id);
  use.setAttributeNS(XLINK_NS, 'xlink:href', '#ic-' + id); // Safari fallback
  svg.appendChild(use);
  return svg;
}

// One muted empty-state: a small icon over a short line.
function makeEmptyState(iconId, text) {
  const box = document.createElement('div');
  box.className = 'files-empty';
  box.appendChild(svgIcon(iconId));
  const t = document.createElement('div');
  t.className = 'files-empty-text';
  t.textContent = text;
  box.appendChild(t);
  return box;
}

// One skeleton pattern for list loading. Returns markup (string) so it can drop
// straight into innerHTML like the old "loading…" line it replaces.
function skeletonHTML(rows = 4) {
  let s = '<div class="files-skeleton">';
  for (let i = 0; i < rows; i++) s += '<div class="skel-row"></div>';
  return s + '</div>';
}

function getFavorites() { return lsGet(FAVORITES_KEY, []); }
function setFavorites(v) { lsSet(FAVORITES_KEY, v); }
function getRecents() { return lsGet(RECENTS_KEY, []); }
function setRecents(v) { lsSet(RECENTS_KEY, v); }

// ---- Folder History storage (new key; migrates legacy files.recents once) ----
const HISTORY_KEY = 'files.history';
// Reads the history list, migrating the legacy recents key on first access and
// persisting the migrated shape so the old key is no longer consulted.
function getHistory() {
  const stored = lsGet(HISTORY_KEY, null);
  if (Array.isArray(stored)) return migrateHistory(stored, null);
  const migrated = migrateHistory(null, lsGet(RECENTS_KEY, null));
  if (migrated.length) lsSet(HISTORY_KEY, migrated);
  return migrated;
}
function setHistory(v) { lsSet(HISTORY_KEY, v); }
// Records a visit to `path` (dedupe + timestamp + cap), keeping the legacy
// recents key in loose sync so nothing that still reads it breaks.
function recordHistoryVisit(path) {
  if (!path) return;
  const next = updateHistory(getHistory(), path, Date.now());
  setHistory(next);
  setRecents(updateRecents(getRecents(), path)); // legacy mirror (harmless)
}

// Persists where the user last was in the Files tab (browser path, or the
// Trash view) so a refresh/reopen lands back there instead of the roots
// list. See nextLastLocation/resolveRestoreLocation in files-ui-pure above.
function saveLastLocation(view, path) { lsSet(LAST_LOCATION_KEY, nextLastLocation(view, path)); }
function loadLastLocation() { return resolveRestoreLocation(lsGet(LAST_LOCATION_KEY, null)); }
function clearLastLocation() { lsSet(LAST_LOCATION_KEY, nextLastLocation('browser', null)); }

// ---- apiFetch: single choke point for header injection / 401 / errors ----
// On 401, shows the PIN overlay and returns a promise that resolves once the
// user unlocks and the ORIGINAL request has been retried -- callers just
// `await apiFetch(...)` once and get the eventual real response.

let pendingPinResolvers = [];
let pinBuffer = '';
let pinAttempted = false; // a PIN was submitted; a re-show means it was wrong
let pinLength = 6; // updated from the server's 401 body (pinLength field)

function renderPinDots() {
  // Rebuild if the configured PIN length differs from the markup default.
  if (pinDots.children.length !== pinLength) {
    pinDots.innerHTML = '';
    for (let i = 0; i < pinLength; i++) {
      const d = document.createElement('span');
      d.className = 'pin-dot';
      pinDots.appendChild(d);
    }
  }
  const dots = pinDots.children;
  for (let i = 0; i < dots.length; i++) {
    dots[i].classList.toggle('filled', i < pinBuffer.length);
  }
}

function showPinOverlay() {
  const wasWrong = pinAttempted;
  pinAttempted = false;
  pinBuffer = '';
  renderPinDots();
  if (pinOverlay.hidden) {
    pinOverlay.hidden = false;
  }
  if (wasWrong) {
    pinSub.textContent = 'Wrong PIN — try again';
    pinDots.classList.remove('shake');
    // Force a reflow so the animation restarts on repeated wrong entries.
    void pinDots.offsetWidth;
    pinDots.classList.add('shake');
    if (navigator.vibrate) navigator.vibrate(80);
  } else {
    pinSub.textContent = ' ';
  }
}
function hidePinOverlay() {
  pinOverlay.hidden = true;
}

function submitPin() {
  lsSet(PIN_KEY, pinBuffer);
  pinAttempted = true;
  hidePinOverlay();
  pinBuffer = '';
  renderPinDots();
  const resolvers = pendingPinResolvers;
  pendingPinResolvers = [];
  resolvers.forEach((r) => r());
}

pinPad.addEventListener('click', (e) => {
  const key = e.target.closest('.pin-key');
  if (!key) return;
  if (key.dataset.del) {
    pinBuffer = pinBuffer.slice(0, -1);
    renderPinDots();
    return;
  }
  if (pinBuffer.length >= pinLength) return;
  pinBuffer += key.dataset.digit;
  renderPinDots();
  if (pinBuffer.length === pinLength) {
    // Let the 6th dot paint before the overlay goes away.
    setTimeout(submitPin, 140);
  }
});

async function apiFetch(url, opts = {}) {
  const pin = lsGet(PIN_KEY, '') || '';
  const headers = Object.assign({}, opts.headers, { 'x-bridge-pin': pin });
  if (opts.body && !headers['Content-Type']) headers['Content-Type'] = 'application/json';

  let res;
  try {
    res = await fetch(url, Object.assign({}, opts, { headers }));
  } catch (e) {
    toast(friendlyError(e));
    throw e;
  }

  if (res.status === 401) {
    try {
      const body = await res.json();
      if (body && body.pinLength >= 4 && body.pinLength <= 12) pinLength = body.pinLength;
    } catch (_) { /* keep the default length */ }
    // Wait for the user to unlock, then retry this exact request once.
    await new Promise((resolve) => {
      pendingPinResolvers.push(resolve);
      showPinOverlay();
    });
    return apiFetch(url, opts);
  }

  if (!res.ok) {
    let msg = `error ${res.status}`;
    try {
      const body = await res.json();
      if (body && body.error) msg = body.error;
    } catch (_) { /* non-JSON error body */ }
    toast(msg);
    const err = new Error(msg);
    err.status = res.status;
    throw err;
  }

  return res.json();
}

// ---- Generic prompt / confirm overlays (used by rename / new folder / delete / kill) --

function showPrompt(title, initialValue) {
  return new Promise((resolve) => {
    promptTitle.textContent = title;
    promptInput.value = initialValue || '';
    promptOverlay.hidden = false;
    setTimeout(() => { promptInput.focus(); promptInput.select(); }, 30);

    const cleanup = () => {
      promptOverlay.hidden = true;
      promptOkBtn.removeEventListener('click', onOk);
      promptCancelBtn.removeEventListener('click', onCancel);
    };
    const onOk = () => { const v = promptInput.value.trim(); cleanup(); resolve(v || null); };
    const onCancel = () => { cleanup(); resolve(null); };
    promptOkBtn.addEventListener('click', onOk);
    promptCancelBtn.addEventListener('click', onCancel);
  });
}

function showConfirm(title) {
  return new Promise((resolve) => {
    confirmTitle.textContent = title;
    confirmOverlay.hidden = false;

    const cleanup = () => {
      confirmOverlay.hidden = true;
      confirmOkBtn.removeEventListener('click', onOk);
      confirmCancelBtn.removeEventListener('click', onCancel);
    };
    const onOk = () => { cleanup(); resolve(true); };
    const onCancel = () => { cleanup(); resolve(false); };
    confirmOkBtn.addEventListener('click', onOk);
    confirmCancelBtn.addEventListener('click', onCancel);
  });
}

// ---- Files browser state ---------------------------------------------------

let filesTabActive = false;
let filesScope = 'roots'; // 'roots' | 'profile' | 'drives' -- from /api/roots
let filesEffectiveRoots = [];
let filesShortcuts = []; // cached from /api/roots -- no extra fetches for the Quick access row
let filesCurrentPath = null; // null = showing the roots list
let filesCurrentParent = null;
let filesCurrentEntries = [];
const gitStatusCache = {}; // path -> dirty bool, populated lazily per session
let sessionsRefreshTimer = null;
let sessionsData = [];
let filesInTrashView = false;
let filesInHistoryView = false;

function filesFormatUptime(startedAtMs) {
  const secs = Math.max(0, Math.floor((Date.now() - startedAtMs) / 1000));
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m`;
  const hrs = Math.floor(mins / 60);
  return `${hrs}h ${mins % 60}m`;
}

function filesPathTail(p) {
  const parts = String(p).split(/[\\/]+/).filter(Boolean);
  return parts.length ? parts[parts.length - 1] : p;
}

async function loadRoots() {
  const data = await apiFetch('/api/roots');
  filesScope = data.scope || 'roots';
  filesEffectiveRoots = data.effectiveRoots || [];
  filesShortcuts = data.shortcuts || [];
  return data;
}

// Switch how much of the PC the Files tab can browse. Persists server-side and
// re-renders the roots list with the new effective roots.
async function setScope(scope) {
  if (scope === filesScope) return;
  try {
    const data = await apiFetch('/api/scope', {
      method: 'POST',
      body: JSON.stringify({ scope }),
    });
    filesScope = data.scope || scope;
    filesEffectiveRoots = data.effectiveRoots || [];
    renderEntryList();
  } catch (_) { /* toast shown by apiFetch */ }
}

// Two-tap kill state, keyed by session id, held in JS (NEVER in the DOM) so a
// full re-render can't disarm it or eat the first tap. renderSessions()
// re-applies the armed CSS class from this map on every rebuild. A per-id
// timer reverts the armed state after the window elapses.
const KILL_WINDOW_MS = 3000;
const killArmed = new Map();   // id -> { armed: true, armedAt: number }
const killTimers = new Map();  // id -> setTimeout handle
const killingIds = new Set();  // ids whose kill POST is in flight / done (optimistic hide)

// Any session's kill button being armed pauses the auto re-render, so a tap can
// never straddle a node replacement.
function anyKillArmed() { return killArmed.size > 0; }

function disarmKill(id, rerender = true) {
  killArmed.delete(id);
  const t = killTimers.get(id);
  if (t) { clearTimeout(t); killTimers.delete(id); }
  if (rerender) renderSessionsList();
}

function armKill(id) {
  const now = Date.now();
  const { state } = twoTapReduce({ armed: false, armedAt: 0 }, 'tap', now, KILL_WINDOW_MS);
  killArmed.set(id, state);
  const prev = killTimers.get(id);
  if (prev) clearTimeout(prev);
  killTimers.set(id, setTimeout(() => {
    // Only revert if the window has genuinely elapsed for THIS arming.
    const cur = killArmed.get(id);
    if (!cur) return;
    const r = twoTapReduce(cur, 'timeout', Date.now(), KILL_WINDOW_MS);
    if (r.action === 'revert') disarmKill(id);
  }, KILL_WINDOW_MS + 30));
  renderSessionsList();
}

async function performKill(id, onDone) {
  disarmKill(id, false);
  killingIds.add(id);
  renderSessionsList();
  try {
    await apiFetch('/api/kill', { method: 'POST', body: JSON.stringify({ id }) });
    // Optimistically drop the row now, then refetch immediately (don't wait 10s).
    sessionsData = sessionsData.filter((s) => s.id !== id);
    renderSessionsList();
    if (onDone) onDone();
    renderSessions();
  } catch (_) {
    // Restore: clear the killing flag and re-render (toast already shown).
    killingIds.delete(id);
    renderSessionsList();
  }
}

async function renderSessions() {
  try {
    const data = await apiFetch('/api/sessions');
    sessionsData = data.sessions || [];
  } catch (_) {
    return; // toast already shown by apiFetch
  }
  // Drop stale killing flags for ids the server no longer reports.
  const live = new Set(sessionsData.map((s) => s.id));
  for (const id of Array.from(killingIds)) if (!live.has(id)) killingIds.delete(id);
  renderSessionsList();
}

// Pure DOM rebuild from sessionsData + the JS-held armed/killing maps. Contains
// NO per-button listeners -- all clicks are handled by the single delegated
// listener on sessionsListEl (wired once at startup).
function renderSessionsList() {
  if (!sessionsData.length) {
    sessionsListEl.innerHTML = '<div class="files-muted-line">No running sessions</div>';
    return;
  }
  sessionsListEl.innerHTML = '';
  sessionsData.forEach((s) => {
    if (killingIds.has(s.id)) return; // optimistically hidden
    const row = document.createElement('div');
    row.className = 'session-row' + (s.alive ? '' : ' dead');

    // Status dot: green (live) when alive, gray otherwise. Replaces the loud pill.
    const dot = document.createElement('span');
    dot.className = 'session-dot' + (s.alive ? ' live' : '');
    row.appendChild(dot);

    const info = document.createElement('div');
    info.className = 'session-info';
    info.dataset.action = 'peek';
    info.dataset.id = s.id;
    const nameEl = document.createElement('div');
    nameEl.className = 'session-name';
    nameEl.textContent = s.name || filesPathTail(s.path);
    const metaEl = document.createElement('div');
    metaEl.className = 'session-meta';
    metaEl.textContent = s.alive
      ? `${filesPathTail(s.path)} · ${filesFormatUptime(s.startedAtMs)}`
      : `${filesPathTail(s.path)} · gone`;
    info.appendChild(nameEl);
    info.appendChild(metaEl);
    row.appendChild(info);

    // Only "hidden" earns a whisper-quiet tag now; visible is implied by the dot.
    if (s.hidden) {
      const tag = document.createElement('span');
      tag.className = 'session-tag';
      tag.textContent = 'hidden';
      row.appendChild(tag);
    }

    if (s.alive) {
      const killBtn = document.createElement('button');
      killBtn.type = 'button';
      const armed = killArmed.has(s.id);
      killBtn.className = 'session-kill-btn' + (armed ? ' armed' : '');
      killBtn.textContent = armed ? 'sure?' : 'Kill';
      killBtn.dataset.action = 'kill';
      killBtn.dataset.id = s.id;
      row.appendChild(killBtn);
    }

    sessionsListEl.appendChild(row);
  });
}

// Single delegated click listener on the sessions container (which is NEVER
// replaced). Wrapped in try/catch that toasts errors so an iOS throw surfaces
// on the phone instead of dying silently. This is the fix for taps landing on
// re-rendered/detached button nodes.
sessionsListEl.addEventListener('click', (e) => {
  try {
    const target = e.target.closest('[data-action]');
    if (!target) return;
    const action = target.dataset.action;
    const id = target.dataset.id;
    if (action === 'kill') {
      if (killArmed.has(id)) {
        performKill(id);
      } else {
        armKill(id);
      }
    } else if (action === 'peek') {
      openPeek(id);
    }
  } catch (err) {
    toast(friendlyError(err));
  }
});

// The 10s refresh interval runs ONLY while the Files tab is the active mode
// AND the page is visible -- mirrors the existing visibilitychange-gated
// patterns for Bridge/PTT sockets above. Paused while a kill button is armed so
// a tap can never straddle a node replacement.
function startSessionsRefresh() {
  stopSessionsRefresh();
  sessionsRefreshTimer = setInterval(() => {
    if (anyKillArmed()) return;
    if (filesTabActive && document.visibilityState === 'visible') renderSessions();
  }, 10000);
}
function stopSessionsRefresh() {
  if (sessionsRefreshTimer) { clearInterval(sessionsRefreshTimer); sessionsRefreshTimer = null; }
}

// Helper: builds a chip button with a leading icon + label span.
function makeChip(cls, iconId, label) {
  const chip = document.createElement('button');
  chip.type = 'button';
  chip.className = 'chip ' + cls;
  chip.appendChild(svgIcon(iconId));
  const span = document.createElement('span');
  span.className = 'chip-label';
  span.textContent = label;
  chip.appendChild(span);
  return chip;
}

// One merged chip shelf: favorites (starred) lead, then quick-access shortcuts,
// then Trash last. Only shown at the roots screen. The Recent section (rows) is
// rendered separately by renderRecentSection.
function renderChips() {
  const favorites = getFavorites();
  const atRoot = filesCurrentPath === null && !filesInTrashView && !filesInHistoryView;

  chipsShortcutsEl.innerHTML = '';
  if (!atRoot) {
    chipsShortcutsEl.hidden = true;
    renderRecentSection(false);
    return;
  }

  // Favorites first (star carries the meaning).
  favorites.forEach((p) => {
    const chip = makeChip('chip-fav', 'star', filesPathTail(p));
    chip.addEventListener('click', () => navigateTo(p));
    chipsShortcutsEl.appendChild(chip);
  });

  // Quick-access shortcuts + trailing Trash pseudo-chip.
  buildShortcutChips(filesShortcuts).forEach((s) => {
    const chip = s.isTrash
      ? makeChip('chip-trash', 'trash', 'Trash')
      : makeChip('chip-shortcut', 'folder', s.name);
    chip.addEventListener('click', () => (s.isTrash ? openTrashView() : navigateTo(s.path)));
    chipsShortcutsEl.appendChild(chip);
  });

  chipsShortcutsEl.hidden = chipsShortcutsEl.children.length === 0;
  renderRecentSection(true);
}

// Root "Recent" section: top-5 history rows + a "see all" affordance into the
// full History sub-view. `atRoot` gates visibility (hidden everywhere else).
function renderRecentSection(atRoot) {
  const history = getHistory();
  if (!atRoot || !history.length) {
    recentHeadEl.hidden = true;
    recentListEl.hidden = true;
    recentListEl.innerHTML = '';
    return;
  }
  recentHeadEl.hidden = false;
  recentListEl.hidden = false;
  recentSeeAllBtn.hidden = history.length <= 5;
  renderHistoryRows(recentListEl, history.slice(0, 5));
}

// Renders history rows into a container using the shared event-delegation
// pattern (data-action/data-path on rows; a single listener per container).
function renderHistoryRows(container, entries) {
  container.innerHTML = '';
  const now = Date.now();
  entries.forEach((entry) => {
    const m = historyEntryModel(entry, now);
    const row = document.createElement('div');
    row.className = 'history-row';
    row.dataset.action = 'nav';
    row.dataset.path = m.path;

    row.appendChild(svgIcon('folder', 'history-icon'));

    const wrap = document.createElement('div');
    wrap.className = 'history-namewrap';
    const name = document.createElement('div');
    name.className = 'history-name';
    name.textContent = m.name;
    wrap.appendChild(name);
    if (m.parent) {
      const sub = document.createElement('div');
      sub.className = 'history-sub';
      sub.textContent = m.parent;
      wrap.appendChild(sub);
    }
    row.appendChild(wrap);

    if (m.ago) {
      const ago = document.createElement('span');
      ago.className = 'history-ago';
      ago.textContent = m.ago;
      row.appendChild(ago);
    }

    const spawnBtn = document.createElement('button');
    spawnBtn.type = 'button';
    spawnBtn.className = 'entry-spawn-btn';
    spawnBtn.appendChild(svgIcon('play'));
    spawnBtn.dataset.action = 'spawn';
    spawnBtn.dataset.path = m.path;
    row.appendChild(spawnBtn);

    container.appendChild(row);
  });
}

// Shows/hides the browsing-header controls (Start-here, New folder/Upload/Sort
// row, and the filter/search inputs). They appear only when browsing INSIDE a
// folder (not at the roots list, not in Trash, not while a deep search is
// showing results). Called from renderBreadcrumbs and the trash toggles.
function updateBrowsingChrome() {
  const inFolder = filesCurrentPath !== null && !filesInTrashView && !filesInHistoryView && !searchActive;
  filesStartHereBtn.hidden = !inFolder;
  filesHeaderRowEl.hidden = !inFolder;
  filesSearchRowEl.hidden = !inFolder;
  document.getElementById('files-sort-label').textContent = getFilesSort() === 'modified' ? 'Modified' : 'Name';
  updateSearchAllRow();
}

function renderBreadcrumbs() {
  breadcrumbBarEl.innerHTML = '';
  if (filesCurrentPath === null) {
    breadcrumbRowEl.hidden = true;
    updateBrowsingChrome();
    return;
  }
  breadcrumbRowEl.hidden = false;

  const upBtn = document.createElement('button');
  upBtn.type = 'button';
  upBtn.className = 'crumb-btn';
  upBtn.appendChild(svgIcon('chevron-left'));
  upBtn.appendChild(document.createTextNode('Roots'));
  upBtn.addEventListener('click', () => navigateTo(null));
  breadcrumbBarEl.appendChild(upBtn);

  const sep0 = document.createElement('span');
  sep0.className = 'crumb-sep';
  sep0.textContent = '/';
  breadcrumbBarEl.appendChild(sep0);

  const crumbs = filesBreadcrumbs(filesCurrentPath, filesEffectiveRoots);
  crumbs.forEach((c, i) => {
    const btn = document.createElement('button');
    btn.type = 'button';
    // Deepest crumb = where you are: highlight it (accent).
    btn.className = 'crumb-btn' + (i === crumbs.length - 1 ? ' crumb-current' : '');
    btn.textContent = c.label;
    btn.addEventListener('click', () => navigateTo(c.path));
    breadcrumbBarEl.appendChild(btn);
    if (i < crumbs.length - 1) {
      const sep = document.createElement('span');
      sep.className = 'crumb-sep';
      sep.textContent = '/';
      breadcrumbBarEl.appendChild(sep);
    }
  });

  // Keep the tail (current folder) visible on deep paths.
  breadcrumbBarEl.scrollLeft = breadcrumbBarEl.scrollWidth;

  renderCurrentFavBtn();
  updateBrowsingChrome();
}

function renderCurrentFavBtn() {
  if (filesCurrentPath === null) return;
  const norm = (x) => String(x).toLowerCase().replace(/[\\/]+$/, '');
  const isFav = getFavorites().some((f) => norm(f) === norm(filesCurrentPath));
  currentFavBtn.classList.toggle('fav-on', isFav);
}

currentFavBtn.addEventListener('click', () => {
  if (filesCurrentPath === null) return;
  const norm = (x) => String(x).toLowerCase().replace(/[\\/]+$/, '');
  const favorites = getFavorites();
  const isFav = favorites.some((f) => norm(f) === norm(filesCurrentPath));
  const next = isFav
    ? favorites.filter((f) => norm(f) !== norm(filesCurrentPath))
    : [filesCurrentPath, ...favorites];
  setFavorites(next);
  renderCurrentFavBtn();
  toast(isFav ? 'removed from favorites' : 'added to favorites');
});

function fetchGitStatusLazily(entries) {
  // Sequential-ish, capped at ~3 in flight, cached per path for the session.
  const targets = entries.filter((e) => e.isRepo && !(e.path in gitStatusCache));
  let idx = 0;
  const worker = async () => {
    while (idx < targets.length) {
      const e = targets[idx++];
      try {
        const data = await apiFetch(`/api/gitstatus?path=${encodeURIComponent(e.path)}`);
        gitStatusCache[e.path] = !!data.dirty;
        renderEntryList(); // refresh badges as results trickle in
      } catch (_) {
        gitStatusCache[e.path] = false; // avoid retry storms on repeated errors
      }
    }
  };
  const workerCount = Math.min(3, targets.length);
  for (let i = 0; i < workerCount; i++) worker();
}

// Index of the entry currently rendered at each row, keyed by a stable path,
// so the single delegated listener can resolve a clicked row back to its entry
// without per-row closures.
let entryByPath = new Map();

// Segmented control for the browse scope, shown above the roots list. Tapping a
// segment POSTs /api/scope and re-renders.
function renderScopeSelector() {
  const wrap = document.createElement('div');
  wrap.className = 'scope-selector';
  const opts = [
    { key: 'roots', label: 'Folders' },
    { key: 'profile', label: 'Profile' },
    { key: 'drives', label: 'All drives' },
  ];
  opts.forEach((o) => {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'scope-seg' + (filesScope === o.key ? ' active' : '');
    btn.textContent = o.label;
    btn.addEventListener('click', () => setScope(o.key));
    wrap.appendChild(btn);
  });
  return wrap;
}

function renderEntryList() {
  entryListEl.innerHTML = '';
  entryByPath = new Map();

  if (filesCurrentPath === null) {
    // Scope selector: how much of the PC is browsable. "All drives" exposes the
    // whole machine; "Profile" is C:\Users\<you>; "Roots" the configured folders.
    entryListEl.appendChild(renderScopeSelector());
    // Roots list -- each root row shows a disk-space line when known.
    filesEffectiveRoots.forEach((r) => {
      const row = document.createElement('div');
      row.className = 'entry-row';
      row.dataset.action = 'nav';
      row.dataset.path = r.path;
      entryByPath.set(r.path, { path: r.path, isDir: true });

      row.appendChild(svgIcon('folder', 'entry-icon folder'));

      const wrap = document.createElement('div');
      wrap.className = 'entry-namewrap';
      wrap.style.flex = '1';
      wrap.style.minWidth = '0';
      const name = document.createElement('div');
      name.className = 'entry-name';
      name.textContent = r.name;
      wrap.appendChild(name);
      const disk = formatDiskSpace(r.freeBytes, r.totalBytes);
      if (disk) {
        const sub = document.createElement('div');
        sub.className = 'entry-sub';
        sub.textContent = disk;
        wrap.appendChild(sub);
      }
      row.appendChild(wrap);
      entryListEl.appendChild(row);
    });
    if (!filesEffectiveRoots.length) {
      entryListEl.innerHTML = '<div class="files-muted-line">No roots configured</div>';
    }
    return;
  }

  // ".." row.
  const upRow = document.createElement('div');
  upRow.className = 'entry-row entry-up';
  upRow.dataset.action = 'up';
  upRow.appendChild(svgIcon('chevron-left', 'entry-icon'));
  const upLabel = document.createElement('div');
  upLabel.className = 'entry-name';
  upLabel.textContent = filesCurrentParent === null ? '.. (roots)' : '..';
  upRow.appendChild(upLabel);
  entryListEl.appendChild(upRow);

  const favorites = getFavorites();
  const filter = filesSearchInput.value.trim().toLowerCase();
  let annotated = annotateEntries(filesCurrentEntries, gitStatusCache, favorites);
  annotated.sort(filesSortComparator(getFilesSort()));
  if (filter) annotated = annotated.filter((e) => e.name.toLowerCase().includes(filter));

  annotated.forEach((e) => {
    const row = document.createElement('div');
    row.className = 'entry-row' + (e.isDir ? '' : ' entry-file-tappable');
    row.dataset.action = e.isDir ? 'nav' : 'open';
    row.dataset.path = e.path;
    entryByPath.set(e.path, e);

    // Leading type icon: folders bright, files dim (styled by .entry-icon.folder).
    row.appendChild(svgIcon(e.isDir ? 'folder' : 'file', 'entry-icon' + (e.isDir ? ' folder' : '')));

    const name = document.createElement('div');
    name.className = 'entry-name';
    name.textContent = e.name;
    row.appendChild(name);

    // A folder that is really a resolved .lnk shortcut gets a small link glyph.
    if (e.isShortcut) {
      const badge = document.createElement('span');
      badge.className = 'entry-shortcut-badge';
      badge.appendChild(svgIcon('link', 'ic-sm'));
      row.appendChild(badge);
    }

    if (e.favorite) {
      const star = document.createElement('span');
      star.className = 'entry-fav-star';
      star.appendChild(svgIcon('star', 'ic-sm'));
      row.appendChild(star);
    }

    if (e.isRepo) {
      const chip = document.createElement('span');
      chip.className = 'git-chip tappable' + (e.dirty ? ' dirty' : '');
      // Dirty: an amber dot carries the status. Clean: the git-branch glyph.
      chip.appendChild(svgIcon(e.dirty ? 'dot' : 'git'));
      chip.appendChild(document.createTextNode(e.dirty ? 'dirty' : 'git'));
      chip.dataset.action = 'gitglance';
      chip.dataset.path = e.path;
      row.appendChild(chip);
    }

    if (e.isDir) {
      const spawnBtn = document.createElement('button');
      spawnBtn.type = 'button';
      spawnBtn.className = 'entry-spawn-btn';
      spawnBtn.appendChild(svgIcon('play'));
      spawnBtn.dataset.action = 'spawn';
      spawnBtn.dataset.path = e.path;
      row.appendChild(spawnBtn);

      const moreBtn = document.createElement('button');
      moreBtn.type = 'button';
      moreBtn.className = 'entry-more-btn';
      moreBtn.appendChild(svgIcon('more'));
      moreBtn.dataset.action = 'more';
      moreBtn.dataset.path = e.path;
      row.appendChild(moreBtn);
    }

    entryListEl.appendChild(row);
  });

  if (!annotated.length) {
    entryListEl.appendChild(makeEmptyState(
      filter ? 'search' : 'folder',
      filter ? 'No matches in this folder' : 'This folder is empty'
    ));
  }

  fetchGitStatusLazily(filesCurrentEntries);
}

// Long-press detection for spawn buttons: a pointer held >500ms opens the spawn
// options sheet instead of the plain-tap auto spawn. Tracked here (not in the
// delegated click handler) since it needs pointer timing.
let entryLongPressTimer = null;
let entryLongPressFired = false;
let entryLongPressPath = null;

entryListEl.addEventListener('pointerdown', (e) => {
  const spawnBtn = e.target.closest('.entry-spawn-btn');
  if (!spawnBtn) return;
  entryLongPressFired = false;
  entryLongPressPath = spawnBtn.dataset.path;
  entryLongPressTimer = setTimeout(() => {
    entryLongPressFired = true;
    const entry = entryByPath.get(entryLongPressPath);
    openSpawnSheet(entryLongPressPath, entry ? entry.name : null);
  }, 500);
});
function clearEntryLongPress() {
  if (entryLongPressTimer) { clearTimeout(entryLongPressTimer); entryLongPressTimer = null; }
}
entryListEl.addEventListener('pointerup', clearEntryLongPress);
entryListEl.addEventListener('pointercancel', clearEntryLongPress);
entryListEl.addEventListener('pointerleave', clearEntryLongPress);

// Single delegated click listener on the entry list container.
entryListEl.addEventListener('click', (e) => {
  try {
    const target = e.target.closest('[data-action]');
    if (!target) return;
    const action = target.dataset.action;
    const path = target.dataset.path;
    if (action === 'up') { navigateTo(filesCurrentParent === null ? null : filesCurrentParent); return; }
    if (action === 'nav') { navigateTo(path); return; }
    if (action === 'open') { const en = entryByPath.get(path); openViewer(path, en ? en.name : filesPathTail(path)); return; }
    if (action === 'gitglance') { openGitGlance(path); return; }
    if (action === 'more') { const en = entryByPath.get(path); if (en) openActionSheet(en); return; }
    if (action === 'spawn') {
      // A long-press already handled this; suppress the trailing click.
      if (entryLongPressFired) { entryLongPressFired = false; return; }
      quickSpawn(path);
    }
  } catch (err) {
    toast(friendlyError(err));
  }
});

async function navigateTo(targetPath) {
  if (filesInTrashView) closeTrashView(false); // leaving Trash implicitly
  if (filesInHistoryView) closeHistoryView();  // leaving History implicitly
  if (searchActive || filesSearchInput.value) clearSearch(); // fresh folder = fresh search
  renderChips(); // reflect the "leaving root" state immediately

  if (targetPath === null) {
    filesCurrentPath = null;
    filesCurrentParent = null;
    filesCurrentEntries = [];
    renderBreadcrumbs();
    entryListEl.innerHTML = skeletonHTML();
    try {
      await loadRoots();
    } catch (_) { /* toast shown */ }
    saveLastLocation('browser', null);
    renderChips();
    renderBreadcrumbs();
    renderEntryList();
    return;
  }

  entryListEl.innerHTML = skeletonHTML();
  let data;
  try {
    data = await apiFetch(`/api/ls?path=${encodeURIComponent(targetPath)}`);
  } catch (e) {
    if (e && e.status === 404) {
      pruneMissingPath(targetPath);
    }
    // Fall back to roots view rather than leaving a dead loading line up. Also
    // covers 403 (scope changed) -- any failure to load degrades to roots.
    clearLastLocation();
    return navigateTo(null);
  }

  filesCurrentPath = data.path;
  filesCurrentParent = data.parent;
  filesCurrentEntries = data.entries || [];
  recordHistoryVisit(filesCurrentPath);
  saveLastLocation('browser', filesCurrentPath);
  renderChips();
  renderBreadcrumbs();
  renderEntryList();
}

// A favorite/recent path 404s (moved/deleted): toast + remove from the list.
function pruneMissingPath(p) {
  const norm = (x) => String(x).toLowerCase().replace(/[\\/]+$/, '');
  const target = norm(p);
  setFavorites(getFavorites().filter((f) => norm(f) !== target));
  setRecents(getRecents().filter((r) => norm(r) !== target));
  setHistory(getHistory().filter((h) => norm(h.path) !== target));
  toast('that folder is gone — removed from your list');
  renderChips();
}

// ---- Trash view: separate sub-view inside the Files tab, reached from the
// Quick access "🗑 Trash" chip. Fetches fresh every time it's opened (server
// call may take 1-3s per the project brief) and offers Restore-only actions.

let trashItems = [];

// Single source of truth for which primary surface is shown. Driven by the pure
// subViewVisibility() so show/hide stays symmetrical across browser/trash/
// history -- the fix for the old bug where a sub-view's nodes stayed on screen
// after leaving. Every entry/exit path funnels through here.
function applySubViewVisibility() {
  const view = filesInTrashView ? 'trash' : (filesInHistoryView ? 'history' : 'browser');
  const v = subViewVisibility(view);
  // Primary surfaces.
  entryListEl.hidden = !v.entryList;
  trashListEl.hidden = !v.trash;
  historyListEl.hidden = !v.history;
  // Each sub-view's own breadcrumb rides with it.
  trashBreadcrumbRowEl.hidden = !v.trash;
  historyBreadcrumbRowEl.hidden = !v.history;
  // The folder breadcrumb + deep-search results only belong to the browser
  // view, and only when actually browsing a folder / showing results.
  if (v.entryList) {
    searchResultsEl.hidden = !searchActive;
    if (searchActive) entryListEl.hidden = true;
  } else {
    searchResultsEl.hidden = true;
    breadcrumbRowEl.hidden = true;
  }
  // Add the fast enter animation to whichever sub-view just became visible.
  const active = v.trash ? trashListEl : (v.history ? historyListEl : null);
  if (active) {
    active.classList.remove('subview-enter');
    void active.offsetWidth; // restart the animation
    active.classList.add('subview-enter');
  }
}

async function openTrashView() {
  filesInHistoryView = false;
  filesInTrashView = true;
  clearSearch(); // leave any active search behind
  saveLastLocation('trash', null);
  breadcrumbRowEl.hidden = true;
  applySubViewVisibility();
  updateBrowsingChrome(); // hides Start-here/New folder/Upload/Sort/Search
  renderChips(); // hides chips/Recent while in Trash
  await loadAndRenderTrash();
}

// `persist` controls whether we also overwrite the saved last-location --
// false when navigateTo() is already about to save a browser location right
// after, true when the user explicitly backs out via the Trash back button.
function closeTrashView(persist = true) {
  filesInTrashView = false;
  applySubViewVisibility();
  updateBrowsingChrome();
  if (persist) saveLastLocation('browser', filesCurrentPath);
}

trashBackBtn.addEventListener('click', () => navigateTo(filesCurrentPath));

// ---- History sub-view: full recently-visited folder list. Mirrors Trash's
// back-nav + hidden-state discipline exactly (both go through
// applySubViewVisibility so exactly one primary surface is ever visible).

function openHistoryView() {
  filesInTrashView = false;
  filesInHistoryView = true;
  clearSearch();
  breadcrumbRowEl.hidden = true;
  applySubViewVisibility();
  updateBrowsingChrome();
  renderChips();
  renderHistoryRows(historyListEl, getHistory());
  if (!getHistory().length) {
    historyListEl.appendChild(makeEmptyState('clock', 'No folders visited yet'));
  }
}

function closeHistoryView() {
  filesInHistoryView = false;
  applySubViewVisibility();
  updateBrowsingChrome();
}

historyBackBtn.addEventListener('click', () => navigateTo(filesCurrentPath));
recentSeeAllBtn.addEventListener('click', openHistoryView);

// Delegated navigation for both the root Recent rows and the History sub-view.
function historyRowClick(e) {
  try {
    const target = e.target.closest('[data-action]');
    if (!target) return;
    const path = target.dataset.path;
    if (target.dataset.action === 'spawn') {
      if (entryLongPressFired) { entryLongPressFired = false; return; }
      quickSpawn(path);
      return;
    }
    if (target.dataset.action === 'nav') navigateTo(path);
  } catch (err) {
    toast(friendlyError(err));
  }
}
recentListEl.addEventListener('click', historyRowClick);
historyListEl.addEventListener('click', historyRowClick);
// Long-press a history spawn button -> options sheet (same pattern as entries).
function historyLongPressDown(e) {
  const spawnBtn = e.target.closest('.entry-spawn-btn');
  if (!spawnBtn) return;
  entryLongPressFired = false;
  entryLongPressPath = spawnBtn.dataset.path;
  entryLongPressTimer = setTimeout(() => {
    entryLongPressFired = true;
    openSpawnSheet(entryLongPressPath, filesPathTail(entryLongPressPath));
  }, 500);
}
recentListEl.addEventListener('pointerdown', historyLongPressDown);
historyListEl.addEventListener('pointerdown', historyLongPressDown);
recentListEl.addEventListener('pointerup', clearEntryLongPress);
historyListEl.addEventListener('pointerup', clearEntryLongPress);
recentListEl.addEventListener('pointercancel', clearEntryLongPress);
historyListEl.addEventListener('pointercancel', clearEntryLongPress);

async function loadAndRenderTrash() {
  trashListEl.innerHTML = skeletonHTML();
  let data;
  try {
    data = await apiFetch('/api/trash');
  } catch (_) {
    return; // toast already shown by apiFetch
  }
  if (!filesInTrashView) return; // user navigated away while the fetch was in flight
  trashItems = data.items || [];
  renderTrashList();
}

function renderTrashList() {
  trashListEl.innerHTML = '';

  if (!trashItems.length) {
    trashListEl.appendChild(makeEmptyState('trash', 'Trash is empty'));
    return;
  }

  const now = Date.now();
  trashItems.forEach((item) => {
    const display = formatTrashItem(item, now);

    const row = document.createElement('div');
    row.className = 'trash-row' + (display.isDir ? ' trash-dir' : '');

    row.appendChild(svgIcon(display.isDir ? 'folder' : 'file', 'trash-icon'));

    const info = document.createElement('div');
    info.className = 'trash-info';

    const name = document.createElement('div');
    name.className = 'trash-name';
    name.textContent = display.name;
    info.appendChild(name);

    const meta = document.createElement('div');
    meta.className = 'trash-meta';
    meta.textContent = display.originalPath;
    meta.title = display.originalPath;
    info.appendChild(meta);

    const timeLine = document.createElement('div');
    timeLine.className = 'trash-meta';
    timeLine.style.direction = 'ltr';
    timeLine.textContent = display.deletedLabel;
    info.appendChild(timeLine);

    row.appendChild(info);

    if (display.sizeLabel) {
      const size = document.createElement('span');
      size.className = 'trash-size';
      size.textContent = display.sizeLabel;
      row.appendChild(size);
    }

    const restoreBtn = document.createElement('button');
    restoreBtn.type = 'button';
    restoreBtn.className = 'trash-restore-btn';
    restoreBtn.appendChild(svgIcon('restore'));
    restoreBtn.appendChild(document.createTextNode('Restore'));
    restoreBtn.dataset.action = 'restore';
    restoreBtn.dataset.id = display.id;
    row.appendChild(restoreBtn);

    trashListEl.appendChild(row);
  });
}

// Single delegated listener on the trash list container.
trashListEl.addEventListener('click', async (e) => {
  try {
    const btn = e.target.closest('[data-action="restore"]');
    if (!btn) return;
    const id = btn.dataset.id;
    const item = trashItems.find((i) => i.id === id);
    if (!item) return;
    const ok = await showConfirm(`Restore '${item.name}' to ${item.originalPath}?`);
    if (!ok) return;
    try {
      const res = await apiFetch('/api/trash/restore', { method: 'POST', body: JSON.stringify({ id }) });
      toast(`restored to ${res.restoredTo}`);
      trashItems = trashItems.filter((i) => i.id !== id);
      renderTrashList();
    } catch (_) { /* toast already shown */ }
  } catch (err) {
    toast(friendlyError(err));
  }
});

// ---- Action sheet: Start Claude here / New folder / Rename / Delete / Favorite --

let sheetTargetEntry = null;

function openActionSheet(entry) {
  sheetTargetEntry = entry;
  sheetTitle.textContent = entry.name;
  const favorites = getFavorites();
  const norm = (x) => String(x).toLowerCase().replace(/[\\/]+$/, '');
  const isFav = favorites.some((f) => norm(f) === norm(entry.path));
  document.getElementById('sheet-favorite-label').textContent = isFav ? 'Unfavorite' : 'Favorite';
  sheetFavoriteBtn.classList.toggle('fav-on', isFav);
  sheetOverlay.hidden = false;
}
function closeActionSheet() {
  sheetOverlay.hidden = true;
  sheetTargetEntry = null;
}
sheetCancelBtn.addEventListener('click', closeActionSheet);

sheetNewFolderBtn.addEventListener('click', async () => {
  const entry = sheetTargetEntry;
  closeActionSheet();
  if (!entry) return;
  const name = await showPrompt(`New folder inside '${entry.name}'`, '');
  if (!name) return;
  try {
    await apiFetch('/api/mkdir', { method: 'POST', body: JSON.stringify({ parent: entry.path, name }) });
    toast('folder created');
    if (filesCurrentPath !== null) navigateTo(filesCurrentPath);
  } catch (_) { /* toast shown */ }
});

sheetRenameBtn.addEventListener('click', async () => {
  const entry = sheetTargetEntry;
  closeActionSheet();
  if (!entry) return;
  const newName = await showPrompt(`Rename '${entry.name}'`, entry.name);
  if (!newName || newName === entry.name) return;
  try {
    await apiFetch('/api/rename', { method: 'POST', body: JSON.stringify({ path: entry.path, newName }) });
    toast('renamed');
    if (filesCurrentPath !== null) navigateTo(filesCurrentPath);
  } catch (_) { /* toast shown */ }
});

sheetFavoriteBtn.addEventListener('click', () => {
  const entry = sheetTargetEntry;
  closeActionSheet();
  if (!entry) return;
  const norm = (x) => String(x).toLowerCase().replace(/[\\/]+$/, '');
  const favorites = getFavorites();
  const isFav = favorites.some((f) => norm(f) === norm(entry.path));
  const next = isFav ? favorites.filter((f) => norm(f) !== norm(entry.path)) : [entry.path, ...favorites];
  setFavorites(next);
  renderChips();
  toast(isFav ? 'removed from favorites' : 'added to favorites');
});

sheetDeleteBtn.addEventListener('click', async () => {
  const entry = sheetTargetEntry;
  closeActionSheet();
  if (!entry) return;
  const ok = await showConfirm(`Move '${entry.name}' to Recycle Bin?`);
  if (!ok) return;
  try {
    await apiFetch('/api/delete', { method: 'POST', body: JSON.stringify({ path: entry.path }) });
    toast('moved to Recycle Bin');
    if (filesCurrentPath !== null) navigateTo(filesCurrentPath);
  } catch (_) { /* toast shown */ }
});

// ---- New folder in current directory (persistent button) ------------------

filesNewFolderBtn.addEventListener('click', async () => {
  if (filesCurrentPath === null) {
    toast('pick a folder first');
    return;
  }
  const name = await showPrompt('New folder here', '');
  if (!name) return;
  try {
    await apiFetch('/api/mkdir', { method: 'POST', body: JSON.stringify({ parent: filesCurrentPath, name }) });
    toast('folder created');
    navigateTo(filesCurrentPath);
  } catch (_) { /* toast shown */ }
});

// ---- Spawn: one-tap auto, long-press for options ---------------------------
// Plain tap on any spawn button (folder row or the current-folder header) runs
// mode:"auto" with no dialog. Long-press opens the options sheet below.

let spawnTargetPath = null;
let spawnTargetName = null;

// Fires POST /api/spawn and drives the toast from the response, then refetches
// sessions immediately (don't wait for the 10s poll).
async function runSpawn(path, name, mode) {
  if (!path) return;
  try {
    const res = await apiFetch('/api/spawn', {
      method: 'POST',
      body: JSON.stringify({ path, name: name || null, mode }),
    });
    toast(spawnToastText(res || {}));
    recordHistoryVisit(path);
    renderChips();
    renderSessions(); // immediate refetch so the new session shows right away
  } catch (_) { /* toast shown by apiFetch */ }
}

// No name is sent for plain spawns: the Claude app auto-titles the session
// from the first prompt. Only "Custom name…" passes one.
function quickSpawn(path) {
  runSpawn(path, null, 'auto');
}

function openSpawnSheet(path, name) {
  spawnTargetPath = path;
  spawnTargetName = name;
  spawnSheetTitle.textContent = name ? `Start Claude — ${name}` : 'Start Claude';
  spawnOverlay.hidden = false;
}
function closeSpawnSheet() {
  spawnOverlay.hidden = true;
  spawnTargetPath = null;
  spawnTargetName = null;
}
spawnCancelBtn.addEventListener('click', closeSpawnSheet);
spawnOverlay.addEventListener('click', (e) => { if (e.target === spawnOverlay) closeSpawnSheet(); });

spawnVisibleBtn.addEventListener('click', () => {
  const p = spawnTargetPath;
  closeSpawnSheet();
  runSpawn(p, null, 'visible');
});
spawnHiddenBtn.addEventListener('click', () => {
  const p = spawnTargetPath;
  closeSpawnSheet();
  runSpawn(p, null, 'hidden');
});
spawnCustomBtn.addEventListener('click', async () => {
  const p = spawnTargetPath, n = spawnTargetName;
  closeSpawnSheet();
  const name = await showPrompt('Session name', n || '');
  if (name === null) return;
  runSpawn(p, name || null, 'auto');
});

// Prominent "Start Claude here" for the folder being browsed: always ask
// visible/hidden via the options sheet (no silent auto-spawn from here).
filesStartHereBtn.addEventListener('click', () => {
  if (filesCurrentPath === null) return;
  openSpawnSheet(filesCurrentPath, filesPathTail(filesCurrentPath));
});

// ---- Sort toggle -----------------------------------------------------------

filesSortBtn.addEventListener('click', () => {
  const next = getFilesSort() === 'name' ? 'modified' : 'name';
  setFilesSort(next);
  document.getElementById('files-sort-label').textContent = next === 'modified' ? 'Modified' : 'Name';
  renderEntryList();
});

// ---- Search: as-you-type filter + deep "Search all" ------------------------
// The filter narrows the CURRENT folder's entries (re-renders the entry list).
// "Search all" runs /api/search under the effective root and replaces the list
// with results until cleared.

let searchActive = false; // true while deep-search results are showing
let searchDebounceTimer = null;

// Finds the effective root that contains the current path (longest match), so
// deep search is scoped to that root. Falls back to the current folder.
function currentEffectiveRoot() {
  const norm = (p) => String(p).toLowerCase().replace(/[\\/]+$/, '');
  const target = norm(filesCurrentPath || '');
  let best = null;
  for (const r of filesEffectiveRoots) {
    const rn = norm(r.path);
    if (target === rn || target.startsWith(rn + '\\') || target.startsWith(rn + '/')) {
      if (!best || rn.length > norm(best.path).length) best = r;
    }
  }
  return best ? best.path : filesCurrentPath;
}

function updateSearchAllRow() {
  const q = filesSearchInput.value.trim();
  const inFolder = filesCurrentPath !== null && !filesInTrashView && !searchActive;
  if (inFolder && q) {
    filesSearchAllBtn.hidden = false;
    filesSearchAllBtn.innerHTML = '';
    filesSearchAllBtn.appendChild(svgIcon('search'));
    filesSearchAllBtn.appendChild(document.createTextNode(`Search everywhere for "${q}"`));
  } else {
    filesSearchAllBtn.hidden = true;
  }
  filesSearchClearBtn.hidden = !q && !searchActive;
}

function clearSearch() {
  if (searchDebounceTimer) { clearTimeout(searchDebounceTimer); searchDebounceTimer = null; }
  filesSearchInput.value = '';
  searchActive = false;
  searchResultsEl.hidden = true;
  searchResultsEl.innerHTML = '';
  entryListEl.hidden = false;
}

filesSearchInput.addEventListener('input', () => {
  if (searchActive) return; // typing while showing deep results does nothing until cleared
  if (searchDebounceTimer) clearTimeout(searchDebounceTimer);
  searchDebounceTimer = setTimeout(() => {
    renderEntryList();      // client-side filter of the current folder
    updateSearchAllRow();
  }, 150);
  // Toggle the clear button responsively even before the debounce fires.
  updateSearchAllRow();
});

filesSearchClearBtn.addEventListener('click', () => {
  clearSearch();
  renderEntryList();
  updateBrowsingChrome();
});

filesSearchAllBtn.addEventListener('click', () => runDeepSearch());

async function runDeepSearch() {
  const q = filesSearchInput.value.trim();
  if (!q || filesCurrentPath === null) return;
  const root = currentEffectiveRoot();
  searchActive = true;
  entryListEl.hidden = true;
  searchResultsEl.hidden = false;
  searchResultsEl.innerHTML = '<div class="files-loading-line">searching…</div>';
  filesSearchAllBtn.hidden = true;
  filesSearchClearBtn.hidden = false;
  let data;
  try {
    data = await apiFetch(`/api/search?root=${encodeURIComponent(root)}&q=${encodeURIComponent(q)}`);
  } catch (_) {
    // toast shown; drop back to browsing.
    clearSearch();
    renderEntryList();
    return;
  }
  if (!searchActive) return; // cleared while in flight
  renderSearchResults(data.results || [], data.truncated, data.tookMs);
}

let searchByPath = new Map();

function renderSearchResults(results, truncated, tookMs) {
  searchResultsEl.innerHTML = '';
  searchByPath = new Map();

  const count = document.createElement('div');
  count.className = 'search-count';
  let label = `${results.length} result${results.length === 1 ? '' : 's'}`;
  if (typeof tookMs === 'number') label += ` · ${tookMs}ms`;
  if (truncated) label += ' · truncated';
  count.textContent = label;
  searchResultsEl.appendChild(count);

  if (!results.length) {
    searchResultsEl.appendChild(makeEmptyState('search', 'No matches'));
    return;
  }

  results.forEach((r) => {
    const d = searchResultDisplay(r);
    searchByPath.set(d.path, d);
    const row = document.createElement('div');
    row.className = 'entry-row search-row';
    row.dataset.action = d.isDir ? 'nav' : 'open';
    row.dataset.path = d.path;

    const icon = document.createElement('span');
    icon.className = 'search-icon';
    icon.appendChild(svgIcon(d.isDir ? 'folder' : 'file'));
    row.appendChild(icon);

    const wrap = document.createElement('div');
    wrap.className = 'entry-namewrap';
    const name = document.createElement('div');
    name.className = 'entry-name';
    name.textContent = d.name;
    wrap.appendChild(name);
    const sub = document.createElement('div');
    sub.className = 'entry-sub';
    sub.textContent = d.parent;
    wrap.appendChild(sub);
    row.appendChild(wrap);

    searchResultsEl.appendChild(row);
  });
}

searchResultsEl.addEventListener('click', (e) => {
  try {
    const target = e.target.closest('[data-action]');
    if (!target) return;
    const path = target.dataset.path;
    const action = target.dataset.action;
    if (action === 'nav') {
      clearSearch();
      navigateTo(path);
    } else if (action === 'open') {
      const d = searchByPath.get(path);
      openViewer(path, d ? d.name : filesPathTail(path));
    }
  } catch (err) {
    toast(friendlyError(err));
  }
});

// ---- File viewer + download ------------------------------------------------
// Fetches the file as a blob (with the PIN header via a raw fetch, since <img>/
// <a> can't carry it), renders text/image/no-preview, and offers a Download
// that clicks a temporary <a download> off the blob URL.

let viewerBlobUrl = null;

function revokeViewerBlob() {
  if (viewerBlobUrl) { URL.revokeObjectURL(viewerBlobUrl); viewerBlobUrl = null; }
}

function closeViewer() {
  viewerOverlay.hidden = true;
  viewerBodyEl.innerHTML = '';
  revokeViewerBlob();
}
viewerCloseBtn.addEventListener('click', closeViewer);
viewerOverlay.addEventListener('click', (e) => { if (e.target === viewerOverlay) closeViewer(); });

// Drag-down-to-close for the big bottom sheets (peek + viewer). The gesture
// only engages from the grip / header area so it never fights body scrolling;
// a downward drag past the threshold closes. Purely additive to the fat close
// button and backdrop tap.
function bindDragToClose(cardEl, headerEl, closeFn) {
  let startY = null;
  let dy = 0;
  headerEl.addEventListener('touchstart', (e) => {
    if (e.touches.length !== 1) { startY = null; return; }
    startY = e.touches[0].clientY;
    dy = 0;
    cardEl.style.transition = 'none';
  }, { passive: true });
  headerEl.addEventListener('touchmove', (e) => {
    if (startY === null) return;
    dy = Math.max(0, e.touches[0].clientY - startY);
    cardEl.style.transform = `translateY(${dy}px)`;
  }, { passive: true });
  const end = () => {
    if (startY === null) return;
    cardEl.style.transition = '';
    cardEl.style.transform = '';
    const shouldClose = dy > 90;
    startY = null;
    if (shouldClose) closeFn();
  };
  headerEl.addEventListener('touchend', end, { passive: true });
  headerEl.addEventListener('touchcancel', end, { passive: true });
}
bindDragToClose(
  viewerOverlay.querySelector('.viewer-card'),
  viewerOverlay.querySelector('.viewer-head'),
  closeViewer
);

function humanSize(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

// Raw fetch that injects the PIN header (apiFetch parses JSON, so we can't use
// it for binary). Handles 401 by surfacing the PIN overlay and retrying, so a
// locked session still works from the viewer.
async function fetchBlobWithPin(url) {
  const pin = lsGet(PIN_KEY, '') || '';
  const res = await fetch(url, { headers: { 'x-bridge-pin': pin } });
  if (res.status === 401) {
    await new Promise((resolve) => { pendingPinResolvers.push(resolve); showPinOverlay(); });
    return fetchBlobWithPin(url);
  }
  if (!res.ok) {
    let msg = `error ${res.status}`;
    try { const b = await res.json(); if (b && b.error) msg = b.error; } catch (_) {}
    const err = new Error(msg); err.status = res.status; throw err;
  }
  const blob = await res.blob();
  return { blob, contentType: res.headers.get('Content-Type') || '' };
}

async function openViewer(path, name) {
  revokeViewerBlob();
  viewerTitle.textContent = name || filesPathTail(path);
  viewerSub.textContent = 'loading…';
  viewerBodyEl.innerHTML = '<div class="files-loading-line">loading…</div>';
  viewerDownloadBtn.disabled = true;
  viewerOverlay.hidden = false;

  let blob, contentType;
  try {
    ({ blob, contentType } = await fetchBlobWithPin(`/api/download?path=${encodeURIComponent(path)}&disposition=inline`));
  } catch (e) {
    viewerSub.textContent = '';
    viewerBodyEl.innerHTML = '';
    const note = document.createElement('div');
    note.className = 'viewer-note';
    note.textContent = String((e && e.message) || e);
    viewerBodyEl.appendChild(note);
    toast(friendlyError(e));
    return;
  }

  viewerBlobUrl = URL.createObjectURL(blob);
  viewerSub.textContent = humanSize(blob.size);
  viewerDownloadBtn.disabled = false;
  viewerBodyEl.innerHTML = '';

  const ct = contentType.toLowerCase();
  const isText = ct.startsWith('text/') || ct.includes('application/json');
  const isImage = ct.startsWith('image/');

  if (isText) {
    const CAP = 1024 * 1024;
    let text = await blob.slice(0, CAP + 1).text();
    let truncated = false;
    if (text.length > CAP) { text = text.slice(0, CAP); truncated = true; }
    if (blob.size > CAP) truncated = true;
    if (truncated) {
      const note = document.createElement('div');
      note.className = 'viewer-note';
      note.textContent = 'showing first 1MB';
      viewerBodyEl.appendChild(note);
    }
    const pre = document.createElement('pre');
    pre.textContent = text;
    viewerBodyEl.appendChild(pre);
  } else if (isImage) {
    const img = document.createElement('img');
    img.src = viewerBlobUrl;
    img.alt = name || '';
    viewerBodyEl.appendChild(img);
  } else {
    const note = document.createElement('div');
    note.className = 'viewer-note';
    note.textContent = `no preview — ${humanSize(blob.size)}${contentType ? ` · ${contentType}` : ''}`;
    viewerBodyEl.appendChild(note);
  }
}

viewerDownloadBtn.addEventListener('click', () => {
  if (!viewerBlobUrl) return;
  const a = document.createElement('a');
  a.href = viewerBlobUrl;
  a.download = viewerTitle.textContent || 'download';
  document.body.appendChild(a);
  a.click();
  // Keep the blob URL alive until after the click; revoke on a short delay so
  // Safari has finished starting the download.
  setTimeout(() => { document.body.removeChild(a); }, 0);
});

// ---- Upload from phone -----------------------------------------------------

filesUploadBtn.addEventListener('click', () => {
  if (filesCurrentPath === null) { toast('pick a folder first'); return; }
  filesUploadInput.value = ''; // allow re-selecting the same file
  filesUploadInput.click();
});

filesUploadInput.addEventListener('change', async () => {
  const files = Array.from(filesUploadInput.files || []);
  if (!files.length || filesCurrentPath === null) return;
  const fd = new FormData();
  files.forEach((f) => fd.append('files', f, f.name));
  toast(`uploading ${files.length}…`);
  try {
    // Do NOT set Content-Type -- the browser adds the multipart boundary. Use a
    // raw fetch with the PIN header (apiFetch would stamp application/json).
    const pin = lsGet(PIN_KEY, '') || '';
    const res = await fetch(`/api/upload?dir=${encodeURIComponent(filesCurrentPath)}`, {
      method: 'POST',
      headers: { 'x-bridge-pin': pin },
      body: fd,
    });
    if (res.status === 401) {
      await new Promise((resolve) => { pendingPinResolvers.push(resolve); showPinOverlay(); });
      // Retry once after unlock.
      filesUploadInput.dispatchEvent(new Event('change'));
      return;
    }
    if (!res.ok) {
      let msg = `error ${res.status}`;
      try { const b = await res.json(); if (b && b.error) msg = b.error; } catch (_) {}
      toast(msg);
      return;
    }
    const data = await res.json();
    toast(uploadSummaryText(data));
    navigateTo(filesCurrentPath); // refresh the listing
  } catch (e) {
    toast(friendlyError(e));
  }
});

// ---- Git glance sheet ------------------------------------------------------

function closeGitGlance() {
  gitOverlay.hidden = true;
  gitListEl.innerHTML = '';
}
gitCloseBtn.addEventListener('click', closeGitGlance);
gitOverlay.addEventListener('click', (e) => { if (e.target === gitOverlay) closeGitGlance(); });

async function openGitGlance(path) {
  gitTitle.textContent = `Changes — ${filesPathTail(path)}`;
  gitListEl.innerHTML = '<div class="files-loading-line">loading…</div>';
  gitOverlay.hidden = false;
  let data;
  try {
    data = await apiFetch(`/api/gitchanges?path=${encodeURIComponent(path)}`);
  } catch (_) {
    closeGitGlance();
    return; // toast shown
  }
  const files = data.files || [];
  gitListEl.innerHTML = '';
  gitTitle.textContent = `Changes — ${filesPathTail(path)} (${files.length})`;
  if (!files.length) {
    gitListEl.innerHTML = '<div class="files-muted-line">clean</div>';
    return;
  }
  files.forEach((f) => {
    const meta = gitStatusMeta(f.status);
    const row = document.createElement('div');
    row.className = 'git-row';
    const st = document.createElement('span');
    st.className = 'git-status ' + meta.color;
    st.textContent = meta.label;
    row.appendChild(st);
    const p = document.createElement('span');
    p.className = 'git-path';
    p.textContent = f.path;
    row.appendChild(p);
    gitListEl.appendChild(row);
  });
  if (data.truncated) {
    const note = document.createElement('div');
    note.className = 'files-muted-line';
    note.textContent = 'truncated';
    gitListEl.appendChild(note);
  }
}

// ---- Session peek ----------------------------------------------------------
// Live "what's it doing" tail. Refreshes every 5s while open. The Kill control
// here uses the same two-tap inline confirm as the sessions panel.

let peekSessionId = null;
let peekRefreshTimer = null;
let peekKillArmed = false;
let peekKillTimer = null;

function openPeek(id) {
  peekSessionId = id;
  const s = sessionsData.find((x) => x.id === id);
  peekTitle.textContent = s ? (s.name || filesPathTail(s.path)) : 'Session';
  peekBodyEl.innerHTML = '<div class="files-loading-line">loading…</div>';
  resetPeekKill();
  peekKillBtn.hidden = !(s && s.alive);
  peekOverlay.hidden = false;
  loadPeek();
  if (peekRefreshTimer) clearInterval(peekRefreshTimer);
  peekRefreshTimer = setInterval(() => {
    if (!peekOverlay.hidden && document.visibilityState === 'visible') loadPeek();
  }, 5000);
}

function closePeek() {
  peekOverlay.hidden = true;
  peekSessionId = null;
  peekBodyEl.innerHTML = '';
  if (peekRefreshTimer) { clearInterval(peekRefreshTimer); peekRefreshTimer = null; }
  resetPeekKill();
}
peekCloseBtn.addEventListener('click', closePeek);
peekOverlay.addEventListener('click', (e) => { if (e.target === peekOverlay) closePeek(); });
bindDragToClose(
  peekOverlay.querySelector('.peek-card'),
  peekOverlay.querySelector('.peek-head'),
  closePeek
);

async function loadPeek() {
  if (!peekSessionId) return;
  let data;
  try {
    data = await apiFetch(`/api/session-peek?id=${encodeURIComponent(peekSessionId)}`);
  } catch (_) {
    return; // toast shown; keep whatever's on screen
  }
  if (peekOverlay.hidden) return;
  const lines = data.lines || [];
  peekBodyEl.innerHTML = '';
  if (!lines.length) {
    const empty = document.createElement('div');
    empty.className = 'peek-empty';
    empty.textContent = 'no transcript found (yet) — the session may still be starting';
    peekBodyEl.appendChild(empty);
    return;
  }
  lines.forEach((line) => {
    const m = peekLineModel(line);
    const el = document.createElement('div');
    el.className = 'peek-line' + (m.dim ? ' dim' : '');
    if (m.prefix) {
      const pre = document.createElement('span');
      pre.className = 'peek-prefix';
      pre.textContent = m.prefix;
      el.appendChild(pre);
    }
    el.appendChild(document.createTextNode(m.text));
    peekBodyEl.appendChild(el);
  });
  peekBodyEl.scrollTop = peekBodyEl.scrollHeight; // auto-scroll to bottom
}

function resetPeekKill() {
  peekKillArmed = false;
  if (peekKillTimer) { clearTimeout(peekKillTimer); peekKillTimer = null; }
  peekKillBtn.classList.remove('armed');
  peekKillBtn.textContent = 'Kill';
}

peekKillBtn.addEventListener('click', () => {
  try {
    if (!peekSessionId) return;
    if (peekKillArmed) {
      const id = peekSessionId;
      resetPeekKill();
      performKill(id, () => closePeek());
    } else {
      peekKillArmed = true;
      peekKillBtn.classList.add('armed');
      peekKillBtn.textContent = 'sure? — tap to kill';
      if (peekKillTimer) clearTimeout(peekKillTimer);
      peekKillTimer = setTimeout(() => { resetPeekKill(); }, KILL_WINDOW_MS);
    }
  } catch (err) {
    toast(friendlyError(err));
  }
});

// ---- Pull-to-refresh on the files scroll container -------------------------
// Only engages when already at the top; a downward drag past the threshold
// shows a hint and refreshes on release. The container is already exempt from
// the global touchmove blocker, so this doesn't fight normal scrolling.

let ptrStartY = null;
let ptrPulling = false;
let ptrArmed = false;
const PTR_THRESHOLD = 70;

filesScrollEl.addEventListener('touchstart', (e) => {
  if (filesScrollEl.scrollTop <= 0 && e.touches.length === 1) {
    ptrStartY = e.touches[0].clientY;
    ptrPulling = true;
    ptrArmed = false;
  } else {
    ptrPulling = false;
  }
}, { passive: true });

filesScrollEl.addEventListener('touchmove', (e) => {
  if (!ptrPulling || ptrStartY === null) return;
  const dy = e.touches[0].clientY - ptrStartY;
  if (dy <= 0 || filesScrollEl.scrollTop > 0) {
    // Scrolled up or moved off the top -- cancel the gesture.
    ptrPulling = false;
    ptrHintEl.hidden = true;
    ptrArmed = false;
    return;
  }
  ptrHintEl.hidden = false;
  ptrArmed = dy > PTR_THRESHOLD;
  ptrHintEl.textContent = ptrArmed ? 'release to refresh' : 'pull to refresh';
  ptrHintEl.classList.toggle('armed', ptrArmed);
}, { passive: true });

function ptrEnd() {
  if (ptrPulling && ptrArmed) refreshCurrentView();
  ptrPulling = false;
  ptrStartY = null;
  ptrArmed = false;
  ptrHintEl.hidden = true;
  ptrHintEl.classList.remove('armed');
}
filesScrollEl.addEventListener('touchend', ptrEnd, { passive: true });
filesScrollEl.addEventListener('touchcancel', ptrEnd, { passive: true });

// Refreshes whichever view is showing: deep-search, trash, or the browser.
function refreshCurrentView() {
  renderSessions();
  if (searchActive) { runDeepSearch(); return; }
  if (filesInTrashView) { loadAndRenderTrash(); return; }
  navigateTo(filesCurrentPath);
}

// ---- Files tab entry/exit ---------------------------------------------------

let filesModeEverEntered = false;

async function enterFilesMode() {
  filesTabActive = true;
  startSessionsRefresh();
  renderSessions();

  // Only consult the persisted last-location on the FIRST entry into Files
  // mode this page load -- once the user has navigated around in-session,
  // filesCurrentPath/filesInTrashView already reflect reality and re-reading
  // storage would just be redundant (and could stomp a location reached by
  // an in-session action).
  if (!filesModeEverEntered) {
    filesModeEverEntered = true;
    const restore = loadLastLocation();
    if (restore.view === 'trash') {
      await openTrashView();
      return;
    }
    await navigateTo(restore.path);
    return;
  }

  if (filesInTrashView) {
    await loadAndRenderTrash(); // refresh in case it changed while away
  } else {
    await navigateTo(filesCurrentPath); // re-show whatever was last open (or roots on first visit)
  }
}

function exitFilesMode() {
  filesTabActive = false;
  stopSessionsRefresh();
}

// --- voice-ui-pure (pure; tested by tests/voice-ui.test.mjs) ---
// No DOM/globals in here -- same discipline as dictate-ui-pure above.

const VOICE_PROVIDERS = ['none', 'anthropic', 'openai', 'gemini', 'openrouter'];
const VOICE_PROVIDER_LABELS = {
  none: 'None',
  anthropic: 'Claude',
  openai: 'OpenAI',
  gemini: 'Gemini',
  openrouter: 'OpenRouter',
};
const VOICE_HISTORY_CAP_DEFAULT = 200;
const VOICE_PREVIEW_CHARS = 90;

function voiceNormalizeProvider(p) {
  const s = String(p == null ? '' : p);
  return VOICE_PROVIDERS.indexOf(s) === -1 ? 'none' : s;
}

function voiceProviderLabel(p) {
  return VOICE_PROVIDER_LABELS[voiceNormalizeProvider(p)];
}

// The one function that decides what leaves the phone on a settings save.
// It is deliberately a whitelist rebuild rather than a copy-with-deletes: an
// API key or a has_key map can never survive a round trip through here, no
// matter what junk the caller hands it.
function voiceSettingsPayload(state) {
  const s = state || {};

  const modes = (Array.isArray(s.modes) ? s.modes : []).map((m) => ({
    id: String((m && m.id) || '').trim(),
    name: String((m && m.name) || '').trim(),
    prompt: String((m && m.prompt) || ''),
    apps: (Array.isArray(m && m.apps) ? m.apps : []).map((a) => String(a).trim()).filter(Boolean),
    use_replacements: !!(m && m.use_replacements),
    use_vocabulary: !!(m && m.use_vocabulary),
    use_context: !!(m && m.use_context),
  })).filter((m) => m.id);

  // An active_mode pointing at a deleted mode would leave the phone with no
  // usable selection, so it falls back to the first mode rather than nothing.
  const ids = modes.map((m) => m.id);
  const wanted = String(s.active_mode == null ? '' : s.active_mode);
  const activeMode = ids.indexOf(wanted) !== -1 ? wanted : (ids.length ? ids[0] : '');

  const replacements = (Array.isArray(s.replacements) ? s.replacements : [])
    .map((r) => ({ from: String((r && r.from) || '').trim(), to: String((r && r.to) || '') }))
    .filter((r) => r.from);

  const vocabulary = [];
  const seenVocab = {};
  (Array.isArray(s.vocabulary) ? s.vocabulary : []).forEach((v) => {
    const t = String(v == null ? '' : v).trim();
    if (!t) return;
    const k = t.toLowerCase();
    if (seenVocab[k]) return;
    seenVocab[k] = true;
    vocabulary.push(t);
  });

  const capRaw = Number(s.history_cap);
  const historyCap = isFinite(capRaw)
    ? Math.min(10000, Math.max(0, Math.round(capRaw)))
    : VOICE_HISTORY_CAP_DEFAULT;

  const ai = s.ai || {};
  return {
    modes,
    active_mode: activeMode,
    auto_mode: !!s.auto_mode,
    replacements,
    vocabulary,
    history_cap: historyCap,
    ai: {
      provider: voiceNormalizeProvider(ai.provider),
      model: String(ai.model == null ? '' : ai.model).trim(),
    },
  };
}

// The API never hands back a key, only whether one is stored -- so the field
// advertises replacement rather than pretending to show a value.
function voiceKeyFieldState(hasKey, provider) {
  const p = voiceNormalizeProvider(provider);
  if (p === 'none') {
    return { enabled: false, saved: false, label: 'No API key needed', placeholder: '' };
  }
  const saved = !!(hasKey && hasKey[p]);
  if (saved) {
    return {
      enabled: true,
      saved: true,
      label: 'Key saved ✓ (replace?)',
      placeholder: 'Enter a new key to replace it',
    };
  }
  return { enabled: true, saved: false, label: 'API key', placeholder: 'Paste your API key' };
}

function voiceModelPlaceholder(defaults, provider) {
  const p = voiceNormalizeProvider(provider);
  if (p === 'none') return '';
  const d = defaults && defaults[p];
  return (typeof d === 'string' && d) ? d : 'provider default';
}

// History timestamps arrive as an epoch, but the unit isn't pinned by the
// contract. Anything below ~1973-in-milliseconds has to be seconds.
function voiceAtMs(at) {
  const t = Number(at);
  if (!isFinite(t) || t <= 0) return NaN;
  return t < 1e11 ? t * 1000 : t;
}

function voiceRelativeTime(at, now) {
  const t = voiceAtMs(at);
  const n = Number(now);
  if (!isFinite(t) || !isFinite(n)) return '';
  const secs = Math.floor((n - t) / 1000);
  if (secs < 10) return 'just now'; // also swallows small clock skew
  if (secs < 60) return secs + 's ago';
  const mins = Math.floor(secs / 60);
  if (mins < 60) return mins + 'm ago';
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return hrs + 'h ago';
  const days = Math.floor(hrs / 24);
  if (days < 7) return days + 'd ago';
  const weeks = Math.floor(days / 7);
  if (weeks < 5) return weeks + 'w ago';
  return Math.floor(days / 30) + 'mo ago';
}

// Compact clip length for a list row (dictateDuration's phrasing is for the
// status line and reads wrong inside a dense list).
function voiceClipLength(seconds) {
  const s = Number(seconds);
  if (!isFinite(s) || s <= 0) return '—';
  if (s < 60) return (Math.round(s * 10) / 10) + 's';
  let mins = Math.floor(s / 60);
  let rem = Math.round(s % 60);
  if (rem === 60) { mins += 1; rem = 0; }
  return rem === 0 ? mins + 'm' : mins + 'm ' + rem + 's';
}

function voicePreview(text, max) {
  const limit = (typeof max === 'number' && max > 0) ? Math.floor(max) : VOICE_PREVIEW_CHARS;
  const flat = String(text == null ? '' : text).replace(/\s+/g, ' ').trim();
  if (!flat) return '(no text)';
  if (flat.length <= limit) return flat;
  // Back off to the last whole word so a preview never ends mid-word -- but
  // only if that still leaves a useful amount of text.
  const cut = flat.slice(0, limit - 1);
  const lastSpace = cut.lastIndexOf(' ');
  const body = lastSpace >= limit * 0.5 ? cut.slice(0, lastSpace) : cut.replace(/\s+$/, '');
  return body + '…';
}

function voiceHistoryRow(entry, now) {
  const e = entry || {};
  const text = typeof e.text === 'string' ? e.text : '';
  const raw = typeof e.raw === 'string' ? e.raw : '';
  return {
    id: String(e.id == null ? '' : e.id),
    when: voiceRelativeTime(e.at, now),
    mode: (typeof e.mode === 'string' && e.mode) ? e.mode : 'raw',
    duration: voiceClipLength(e.seconds),
    preview: voicePreview(text || raw),
  };
}

// A warning means the AI step fell over, NOT that dictation failed: the raw
// transcript still came through and must stay on screen and copyable.
function voiceDoneView(msg) {
  const m = msg || {};
  const text = typeof m.text === 'string' ? m.text : '';
  const raw = typeof m.raw === 'string' ? m.raw : '';
  const warning = (typeof m.warning === 'string' && m.warning.trim()) ? m.warning.trim() : null;
  const mode = (typeof m.mode === 'string' && m.mode.trim()) ? m.mode.trim() : null;
  let state;
  if (text) state = warning ? 'delivered raw transcript' : 'typed into your PC';
  else state = warning || 'nothing heard';
  return {
    show: text.length > 0,
    text,
    raw: (raw && raw !== text) ? raw : null,
    mode,
    notice: warning,
    copyable: text.length > 0,
    state,
  };
}

// Instant narrowing while the server's ?q= round trip is still in flight.
function voiceFilterHistory(entries, q) {
  const list = Array.isArray(entries) ? entries : [];
  const needle = String(q == null ? '' : q).trim().toLowerCase();
  if (!needle) return list.slice();
  return list.filter((e) => {
    if (!e) return false;
    return [e.text, e.raw, e.mode].some(
      (f) => typeof f === 'string' && f.toLowerCase().indexOf(needle) !== -1,
    );
  });
}

function voiceModeChips(modes, activeId) {
  const list = (Array.isArray(modes) ? modes : []).filter((m) => m && m.id);
  const wanted = String(activeId == null ? '' : activeId);
  const hasActive = list.some((m) => String(m.id) === wanted);
  return list.map((m, i) => ({
    id: String(m.id),
    name: String(m.name || m.id),
    active: hasActive ? String(m.id) === wanted : i === 0,
  }));
}

function voiceStatCards(stats) {
  const s = stats || {};
  const n = (v) => {
    const x = Number(v);
    return isFinite(x) && x > 0 ? String(Math.round(x)) : '0';
  };
  return [
    { label: 'dictations', value: n(s.total) },
    { label: 'words this week', value: n(s.words_this_week) },
    { label: 'minutes saved', value: n(s.minutes_saved) },
  ];
}

// Turns a free-typed mode name into a stable id. Suffixed with a short
// timestamp so two modes called "Email" don't collide.
function voiceModeId(name, stamp) {
  const slug = String(name == null ? '' : name)
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 24);
  const suffix = Number(stamp).toString(36).slice(-4);
  return (slug || 'mode') + '-' + suffix;
}

// --- end voice-ui-pure ---

// ---- Voice settings + history views ---------------------------------------
// Both views hang off /api/voice/*. Settings are edited into a local buffer
// (already in PUT shape, so a save can never leak a key) and pushed on Save.

const dictateModesEl = document.getElementById('dictate-modes');
const dictateWarningEl = document.getElementById('dictate-warning');
const dictateModeTagEl = document.getElementById('dictate-mode-tag');

const viewSettings = document.getElementById('view-settings');
const viewHistory = document.getElementById('view-history');

const voiceProviderRowEl = document.getElementById('voice-provider-row');
const voiceModelInput = document.getElementById('voice-model');
const voiceKeyLabelEl = document.getElementById('voice-key-label');
const voiceKeyInput = document.getElementById('voice-key');
const voiceKeySaveBtn = document.getElementById('voice-key-save');
const voiceModesListEl = document.getElementById('voice-modes-list');
const voiceReplListEl = document.getElementById('voice-repl-list');
const voiceVocabInput = document.getElementById('voice-vocab-input');
const voiceVocabListEl = document.getElementById('voice-vocab-list');
const voiceAutoModeBtn = document.getElementById('voice-auto-mode');
const voiceHistoryCapInput = document.getElementById('voice-history-cap');
const voiceSaveBtn = document.getElementById('voice-save');

const voiceStatsEl = document.getElementById('voice-stats');
const voiceSearchInput = document.getElementById('voice-search');
const voiceHistoryListEl = document.getElementById('voice-history-list');
const voiceHistoryClearBtn = document.getElementById('voice-history-clear');

let voiceSettings = null;      // edit buffer, always in PUT shape
let voiceHasKey = {};          // provider -> bool, never a key itself
let voiceDefaultModels = {};
let voiceSettingsLoaded = false;
let voiceModeExpandedId = null;
let voiceHistoryEntries = [];
let voiceHistoryExpandedId = null;
let voiceSearchTimer = null;
const voiceArmed = new Set(); // two-tap confirm keys, same idea as killArmed

function voiceSetValue(el, v) {
  // Never clobber what the user is mid-way through typing.
  if (el && el.value !== v) el.value = v;
}

async function loadVoiceSettings(force) {
  if (voiceSettingsLoaded && !force) return voiceSettings;
  try {
    const data = await apiFetch('/api/voice/settings');
    voiceApplySettings(data);
    voiceSettingsLoaded = true;
  } catch (_) { /* apiFetch already toasted */ }
  return voiceSettings;
}

function voiceApplySettings(data) {
  const d = data || {};
  const ai = d.ai || {};
  voiceHasKey = ai.has_key || {};
  voiceDefaultModels = ai.default_models || {};
  voiceSettings = voiceSettingsPayload(d);
  voiceArmed.clear(); // a half-armed delete must not survive a reload
  renderVoiceModeChips();
  renderVoiceSettings();
}

async function saveVoiceSettings(silent) {
  if (!voiceSettings) return false;
  const payload = voiceSettingsPayload(voiceSettings);
  try {
    const data = await apiFetch('/api/voice/settings', {
      method: 'PUT',
      body: JSON.stringify(payload),
    });
    voiceApplySettings(data);
    if (!silent) toast('settings saved');
    return true;
  } catch (_) {
    return false;
  }
}

// ---- Dictate tab: mode chips ----------------------------------------------

function renderVoiceModeChips() {
  if (!dictateModesEl) return;
  if (!voiceSettings) { dictateModesEl.hidden = true; return; }
  const chips = voiceModeChips(voiceSettings.modes, voiceSettings.active_mode);
  dictateModesEl.innerHTML = '';
  if (!chips.length) {
    const empty = document.createElement('div');
    empty.className = 'files-muted-line';
    empty.textContent = 'No modes yet';
    dictateModesEl.appendChild(empty);
    dictateModesEl.hidden = false;
    return;
  }
  chips.forEach((c) => {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'chip chip-mode' + (c.active ? ' active' : '');
    b.dataset.modeId = c.id;
    b.textContent = c.name;
    dictateModesEl.appendChild(b);
  });
  dictateModesEl.hidden = false;
}

dictateModesEl.addEventListener('click', (e) => {
  try {
    const btn = e.target.closest('[data-mode-id]');
    if (!btn || !voiceSettings) return;
    voiceSettings.active_mode = btn.dataset.modeId;
    renderVoiceModeChips();
    renderVoiceModes();
    saveVoiceSettings(true);
  } catch (err) { toast(friendlyError(err)); }
});

// Called for every dictation-done frame. Returns the view so the caller can
// reuse the phrasing for the status line.
function showDictateDone(msg) {
  const v = voiceDoneView(msg);
  dictateWarningEl.textContent = v.notice || '';
  dictateWarningEl.hidden = !v.notice;
  dictateModeTagEl.textContent = v.mode || '';
  dictateModeTagEl.hidden = !v.mode;
  // A warning with no text still deserves the card -- otherwise the only
  // signal that the AI step died would be a status line that scrolls away.
  if (!v.show && !v.notice) {
    dictateResultEl.hidden = true;
    return v;
  }
  dictateTextEl.textContent = v.text;
  dictateResultEl.hidden = false;
  return v;
}

// ---- Settings: provider + key ---------------------------------------------

function renderVoiceProviders() {
  voiceProviderRowEl.innerHTML = '';
  const current = voiceSettings ? voiceSettings.ai.provider : 'none';
  VOICE_PROVIDERS.forEach((p) => {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'chip chip-mode' + (p === current ? ' active' : '');
    b.dataset.provider = p;
    b.textContent = VOICE_PROVIDER_LABELS[p];
    voiceProviderRowEl.appendChild(b);
  });
}

function renderVoiceKeyField() {
  const provider = voiceSettings ? voiceSettings.ai.provider : 'none';
  const st = voiceKeyFieldState(voiceHasKey, provider);
  voiceKeyLabelEl.textContent = st.label;
  voiceKeyInput.placeholder = st.placeholder;
  voiceKeyInput.disabled = !st.enabled;
  voiceKeySaveBtn.disabled = !st.enabled;
  voiceModelInput.placeholder = voiceModelPlaceholder(voiceDefaultModels, provider);
}

voiceProviderRowEl.addEventListener('click', (e) => {
  try {
    const b = e.target.closest('[data-provider]');
    if (!b || !voiceSettings) return;
    voiceSettings.ai.provider = voiceNormalizeProvider(b.dataset.provider);
    voiceKeyInput.value = '';
    renderVoiceProviders();
    renderVoiceKeyField();
  } catch (err) { toast(friendlyError(err)); }
});

voiceModelInput.addEventListener('input', () => {
  if (voiceSettings) voiceSettings.ai.model = voiceModelInput.value;
});

voiceKeySaveBtn.addEventListener('click', async () => {
  if (!voiceSettings) return;
  const provider = voiceSettings.ai.provider;
  if (provider === 'none') { toast('pick a provider first'); return; }
  const key = voiceKeyInput.value;
  try {
    const res = await apiFetch('/api/voice/key', {
      method: 'POST',
      body: JSON.stringify({ provider, key }),
    });
    if (res && res.has_key) voiceHasKey = res.has_key;
    voiceKeyInput.value = '';
    renderVoiceKeyField();
    toast(key.trim() ? 'key saved' : 'key cleared');
  } catch (_) { /* toasted */ }
});

// ---- Settings: modes -------------------------------------------------------

const VOICE_MODE_FLAGS = [
  { flag: 'use_replacements', label: 'Apply replacements' },
  { flag: 'use_vocabulary', label: 'Apply vocabulary' },
  { flag: 'use_context', label: 'Use screen context' },
];

function voiceToggleButton(label, on, dataset) {
  const b = document.createElement('button');
  b.type = 'button';
  b.className = 'voice-toggle';
  b.setAttribute('aria-pressed', on ? 'true' : 'false');
  Object.keys(dataset).forEach((k) => { b.dataset[k] = dataset[k]; });
  const span = document.createElement('span');
  span.textContent = label;
  const sw = document.createElement('span');
  sw.className = 'voice-switch';
  b.appendChild(span);
  b.appendChild(sw);
  return b;
}

function buildVoiceModeEditor(m) {
  const body = document.createElement('div');
  body.className = 'voice-item-body';

  const nameLabel = document.createElement('div');
  nameLabel.className = 'voice-label';
  nameLabel.textContent = 'Name';
  const nameInput = document.createElement('input');
  nameInput.type = 'text';
  nameInput.className = 'voice-input';
  nameInput.value = m.name;
  nameInput.dataset.field = 'name';
  nameInput.dataset.id = m.id;
  nameInput.autocapitalize = 'off';
  nameInput.spellcheck = false;

  const promptLabel = document.createElement('div');
  promptLabel.className = 'voice-label';
  promptLabel.textContent = 'Prompt';
  const promptInputEl = document.createElement('textarea');
  promptInputEl.className = 'voice-input';
  promptInputEl.value = m.prompt;
  promptInputEl.dataset.field = 'prompt';
  promptInputEl.dataset.id = m.id;

  const appsLabel = document.createElement('div');
  appsLabel.className = 'voice-label';
  appsLabel.textContent = 'Match these apps (comma separated)';
  const appsInput = document.createElement('input');
  appsInput.type = 'text';
  appsInput.className = 'voice-input';
  appsInput.value = m.apps.join(', ');
  appsInput.dataset.field = 'apps';
  appsInput.dataset.id = m.id;
  appsInput.placeholder = 'Slack, Mail, Code';
  appsInput.autocapitalize = 'off';
  appsInput.spellcheck = false;

  body.appendChild(nameLabel);
  body.appendChild(nameInput);
  body.appendChild(promptLabel);
  body.appendChild(promptInputEl);
  body.appendChild(appsLabel);
  body.appendChild(appsInput);

  VOICE_MODE_FLAGS.forEach((f) => {
    body.appendChild(voiceToggleButton(f.label, !!m[f.flag], {
      action: 'mode-toggle', id: m.id, flag: f.flag,
    }));
  });

  const row = document.createElement('div');
  row.className = 'voice-row';
  if (voiceSettings.active_mode !== m.id) {
    const act = document.createElement('button');
    act.type = 'button';
    act.className = 'voice-btn';
    act.dataset.action = 'mode-activate';
    act.dataset.id = m.id;
    act.textContent = 'Make active';
    row.appendChild(act);
  }
  const del = document.createElement('button');
  del.type = 'button';
  del.className = 'voice-btn danger';
  del.dataset.action = 'mode-delete';
  del.dataset.id = m.id;
  del.textContent = voiceArmed.has('mode:' + m.id) ? 'Sure?' : 'Delete';
  if (voiceArmed.has('mode:' + m.id)) del.classList.add('armed');
  row.appendChild(del);
  body.appendChild(row);

  return body;
}

function renderVoiceModes() {
  voiceModesListEl.innerHTML = '';
  const modes = voiceSettings ? voiceSettings.modes : [];
  if (!modes.length) {
    const empty = document.createElement('div');
    empty.className = 'files-muted-line';
    empty.textContent = 'No modes yet';
    voiceModesListEl.appendChild(empty);
    return;
  }
  modes.forEach((m) => {
    const card = document.createElement('div');
    card.className = 'voice-item' + (m.id === voiceSettings.active_mode ? ' active-mode' : '');

    const head = document.createElement('div');
    head.className = 'voice-item-head';
    head.dataset.action = 'mode-expand';
    head.dataset.id = m.id;

    const main = document.createElement('div');
    main.className = 'voice-item-main';
    const name = document.createElement('div');
    name.className = 'voice-item-name';
    name.textContent = m.name || m.id;
    const meta = document.createElement('div');
    meta.className = 'voice-item-meta';
    meta.textContent = m.apps.length ? m.apps.join(', ') : 'any app';
    main.appendChild(name);
    main.appendChild(meta);
    head.appendChild(main);

    if (m.id === voiceSettings.active_mode) {
      const badge = document.createElement('span');
      badge.className = 'voice-badge';
      badge.textContent = 'active';
      head.appendChild(badge);
    }
    card.appendChild(head);
    if (voiceModeExpandedId === m.id) card.appendChild(buildVoiceModeEditor(m));
    voiceModesListEl.appendChild(card);
  });
}

voiceModesListEl.addEventListener('input', (e) => {
  const el = e.target.closest('[data-field]');
  if (!el || !voiceSettings) return;
  const m = voiceSettings.modes.find((x) => x.id === el.dataset.id);
  if (!m) return;
  if (el.dataset.field === 'apps') {
    m.apps = el.value.split(',').map((s) => s.trim()).filter(Boolean);
  } else {
    m[el.dataset.field] = el.value;
  }
});

voiceModesListEl.addEventListener('click', (e) => {
  try {
    const t = e.target.closest('[data-action]');
    if (!t || !voiceSettings) return;
    const id = t.dataset.id;
    const action = t.dataset.action;
    if (action === 'mode-expand') {
      voiceModeExpandedId = voiceModeExpandedId === id ? null : id;
      voiceArmed.delete('mode:' + id);
      renderVoiceModes();
    } else if (action === 'mode-toggle') {
      const m = voiceSettings.modes.find((x) => x.id === id);
      if (!m) return;
      m[t.dataset.flag] = !m[t.dataset.flag];
      t.setAttribute('aria-pressed', m[t.dataset.flag] ? 'true' : 'false');
    } else if (action === 'mode-activate') {
      voiceSettings.active_mode = id;
      renderVoiceModes();
      renderVoiceModeChips();
    } else if (action === 'mode-delete') {
      // Two-tap confirm -- browser modals wedge this app on iOS.
      const key = 'mode:' + id;
      if (!voiceArmed.has(key)) {
        voiceArmed.add(key);
        t.classList.add('armed');
        t.textContent = 'Sure?';
        return;
      }
      voiceArmed.delete(key);
      voiceSettings.modes = voiceSettings.modes.filter((x) => x.id !== id);
      if (voiceSettings.active_mode === id) {
        voiceSettings.active_mode = voiceSettings.modes.length ? voiceSettings.modes[0].id : '';
      }
      voiceModeExpandedId = null;
      renderVoiceModes();
      renderVoiceModeChips();
    }
  } catch (err) { toast(friendlyError(err)); }
});

document.getElementById('voice-mode-add').addEventListener('click', async () => {
  if (!voiceSettings) return;
  const name = await showPrompt('New mode name', '');
  if (!name) return;
  const id = voiceModeId(name, Date.now());
  voiceSettings.modes.push({
    id,
    name,
    prompt: '',
    apps: [],
    use_replacements: true,
    use_vocabulary: true,
    use_context: false,
  });
  if (!voiceSettings.active_mode) voiceSettings.active_mode = id;
  voiceModeExpandedId = id;
  renderVoiceModes();
  renderVoiceModeChips();
});

// ---- Settings: replacements + vocabulary -----------------------------------

function renderVoiceReplacements() {
  voiceReplListEl.innerHTML = '';
  const list = voiceSettings ? voiceSettings.replacements : [];
  if (!list.length) {
    const empty = document.createElement('div');
    empty.className = 'files-muted-line';
    empty.textContent = 'No replacements';
    voiceReplListEl.appendChild(empty);
    return;
  }
  list.forEach((r, i) => {
    const row = document.createElement('div');
    row.className = 'voice-row';
    const from = document.createElement('input');
    from.type = 'text';
    from.className = 'voice-input';
    from.value = r.from;
    from.placeholder = 'heard';
    from.dataset.replField = 'from';
    from.dataset.index = String(i);
    from.autocapitalize = 'off';
    from.spellcheck = false;
    const arrow = document.createElement('span');
    arrow.className = 'voice-label';
    arrow.textContent = '→';
    const to = document.createElement('input');
    to.type = 'text';
    to.className = 'voice-input';
    to.value = r.to;
    to.placeholder = 'typed';
    to.dataset.replField = 'to';
    to.dataset.index = String(i);
    to.autocapitalize = 'off';
    to.spellcheck = false;
    const del = document.createElement('button');
    del.type = 'button';
    del.className = 'voice-btn';
    del.dataset.action = 'repl-del';
    del.dataset.index = String(i);
    del.textContent = '×';
    row.appendChild(from);
    row.appendChild(arrow);
    row.appendChild(to);
    row.appendChild(del);
    voiceReplListEl.appendChild(row);
  });
}

voiceReplListEl.addEventListener('input', (e) => {
  const el = e.target.closest('[data-repl-field]');
  if (!el || !voiceSettings) return;
  const r = voiceSettings.replacements[Number(el.dataset.index)];
  if (!r) return;
  r[el.dataset.replField] = el.value;
});

voiceReplListEl.addEventListener('click', (e) => {
  try {
    const t = e.target.closest('[data-action="repl-del"]');
    if (!t || !voiceSettings) return;
    voiceSettings.replacements.splice(Number(t.dataset.index), 1);
    renderVoiceReplacements();
  } catch (err) { toast(friendlyError(err)); }
});

document.getElementById('voice-repl-add').addEventListener('click', () => {
  if (!voiceSettings) return;
  voiceSettings.replacements.push({ from: '', to: '' });
  renderVoiceReplacements();
});

function renderVoiceVocab() {
  voiceVocabListEl.innerHTML = '';
  const list = voiceSettings ? voiceSettings.vocabulary : [];
  if (!list.length) {
    const empty = document.createElement('div');
    empty.className = 'files-muted-line';
    empty.textContent = 'No vocabulary terms';
    voiceVocabListEl.appendChild(empty);
    return;
  }
  list.forEach((v, i) => {
    const chip = document.createElement('button');
    chip.type = 'button';
    chip.className = 'chip';
    chip.dataset.action = 'vocab-del';
    chip.dataset.index = String(i);
    chip.textContent = v + '  ×';
    voiceVocabListEl.appendChild(chip);
  });
}

voiceVocabListEl.addEventListener('click', (e) => {
  try {
    const t = e.target.closest('[data-action="vocab-del"]');
    if (!t || !voiceSettings) return;
    voiceSettings.vocabulary.splice(Number(t.dataset.index), 1);
    renderVoiceVocab();
  } catch (err) { toast(friendlyError(err)); }
});

function voiceAddVocab() {
  if (!voiceSettings) return;
  const term = voiceVocabInput.value.trim();
  if (!term) return;
  voiceSettings.vocabulary.push(term);
  voiceSettings.vocabulary = voiceSettingsPayload(voiceSettings).vocabulary; // dedupe
  voiceVocabInput.value = '';
  renderVoiceVocab();
}

document.getElementById('voice-vocab-add').addEventListener('click', voiceAddVocab);
voiceVocabInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') { e.preventDefault(); voiceAddVocab(); }
});

// ---- Settings: behaviour + save -------------------------------------------

voiceAutoModeBtn.addEventListener('click', () => {
  if (!voiceSettings) return;
  voiceSettings.auto_mode = !voiceSettings.auto_mode;
  voiceAutoModeBtn.setAttribute('aria-pressed', voiceSettings.auto_mode ? 'true' : 'false');
});

voiceHistoryCapInput.addEventListener('input', () => {
  if (voiceSettings) voiceSettings.history_cap = voiceHistoryCapInput.value;
});

voiceSaveBtn.addEventListener('click', () => { saveVoiceSettings(false); });

function renderVoiceSettings() {
  renderVoiceProviders();
  renderVoiceKeyField();
  if (voiceSettings) {
    voiceSetValue(voiceModelInput, voiceSettings.ai.model);
    voiceSetValue(voiceHistoryCapInput, String(voiceSettings.history_cap));
    voiceAutoModeBtn.setAttribute('aria-pressed', voiceSettings.auto_mode ? 'true' : 'false');
  }
  renderVoiceModes();
  renderVoiceReplacements();
  renderVoiceVocab();
}

// ---- History view ----------------------------------------------------------

async function loadVoiceStats() {
  try {
    const s = await apiFetch('/api/voice/stats');
    voiceStatsEl.innerHTML = '';
    voiceStatCards(s).forEach((c) => {
      const cell = document.createElement('div');
      cell.className = 'voice-stat';
      const num = document.createElement('div');
      num.className = 'voice-stat-num';
      num.textContent = c.value;
      const lab = document.createElement('div');
      lab.className = 'voice-stat-label';
      lab.textContent = c.label;
      cell.appendChild(num);
      cell.appendChild(lab);
      voiceStatsEl.appendChild(cell);
    });
    voiceStatsEl.hidden = false;
  } catch (_) {
    voiceStatsEl.hidden = true;
  }
}

async function loadVoiceHistory() {
  const q = voiceSearchInput.value.trim();
  const url = '/api/voice/history?limit=200' + (q ? '&q=' + encodeURIComponent(q) : '');
  try {
    const data = await apiFetch(url);
    voiceHistoryEntries = Array.isArray(data && data.entries) ? data.entries : [];
  } catch (_) {
    voiceHistoryEntries = [];
  }
  renderVoiceHistory();
}

function voiceCopyBlock(labelText, value) {
  const wrap = document.createElement('div');
  const head = document.createElement('div');
  head.className = 'voice-row';
  const lab = document.createElement('div');
  lab.className = 'voice-label';
  lab.style.flex = '1 1 auto';
  lab.textContent = labelText;
  const copy = document.createElement('button');
  copy.type = 'button';
  copy.className = 'dictate-copy';
  copy.dataset.action = 'copy';
  copy.dataset.text = value;
  copy.textContent = 'copy';
  head.appendChild(lab);
  head.appendChild(copy);
  const block = document.createElement('div');
  block.className = 'voice-text-block';
  block.textContent = value;
  wrap.appendChild(head);
  wrap.appendChild(block);
  return wrap;
}

function buildVoiceHistoryBody(entry) {
  const body = document.createElement('div');
  body.className = 'voice-item-body';
  const text = typeof entry.text === 'string' ? entry.text : '';
  const raw = typeof entry.raw === 'string' ? entry.raw : '';

  body.appendChild(voiceCopyBlock('Processed', text || '(empty)'));
  if (raw && raw !== text) body.appendChild(voiceCopyBlock('Raw transcript', raw));

  const modes = voiceSettings ? voiceSettings.modes : [];
  if (modes.length) {
    const lab = document.createElement('div');
    lab.className = 'voice-label';
    lab.textContent = 'Re-run with';
    body.appendChild(lab);
    const row = document.createElement('div');
    row.className = 'chip-row';
    modes.forEach((m) => {
      const b = document.createElement('button');
      b.type = 'button';
      b.className = 'chip chip-mode';
      b.dataset.action = 'reprocess';
      b.dataset.id = String(entry.id);
      b.dataset.mode = m.id;
      b.textContent = m.name || m.id;
      row.appendChild(b);
    });
    body.appendChild(row);
  }

  const del = document.createElement('button');
  del.type = 'button';
  del.className = 'voice-btn danger';
  del.dataset.action = 'entry-delete';
  del.dataset.id = String(entry.id);
  const armed = voiceArmed.has('entry:' + entry.id);
  del.textContent = armed ? 'Sure?' : 'Delete';
  if (armed) del.classList.add('armed');
  body.appendChild(del);

  return body;
}

function renderVoiceHistory() {
  const entries = voiceFilterHistory(voiceHistoryEntries, voiceSearchInput.value);
  voiceHistoryListEl.innerHTML = '';
  voiceHistoryClearBtn.hidden = voiceHistoryEntries.length === 0;
  if (!entries.length) {
    const empty = document.createElement('div');
    empty.className = 'files-muted-line';
    empty.textContent = voiceSearchInput.value.trim()
      ? 'No dictations match that search'
      : 'No dictations yet';
    voiceHistoryListEl.appendChild(empty);
    return;
  }
  const now = Date.now();
  entries.forEach((entry) => {
    const row = voiceHistoryRow(entry, now);
    const card = document.createElement('div');
    card.className = 'voice-item';

    const head = document.createElement('div');
    head.className = 'voice-item-head';
    head.dataset.action = 'entry-expand';
    head.dataset.id = row.id;

    const main = document.createElement('div');
    main.className = 'voice-item-main';
    const meta = document.createElement('div');
    meta.className = 'voice-item-meta';
    meta.textContent = `${row.when} · ${row.mode} · ${row.duration}`;
    const preview = document.createElement('div');
    preview.className = 'voice-item-preview';
    preview.textContent = row.preview;
    main.appendChild(meta);
    main.appendChild(preview);
    head.appendChild(main);
    card.appendChild(head);

    if (voiceHistoryExpandedId === row.id) card.appendChild(buildVoiceHistoryBody(entry));
    voiceHistoryListEl.appendChild(card);
  });
}

voiceHistoryListEl.addEventListener('click', async (e) => {
  try {
    const t = e.target.closest('[data-action]');
    if (!t) return;
    const action = t.dataset.action;
    const id = t.dataset.id;
    if (action === 'entry-expand') {
      voiceHistoryExpandedId = voiceHistoryExpandedId === id ? null : id;
      voiceArmed.delete('entry:' + id);
      renderVoiceHistory();
    } else if (action === 'copy') {
      navigator.clipboard.writeText(t.dataset.text || '')
        .then(() => toast('copied'))
        .catch(() => toast('copy failed'));
    } else if (action === 'reprocess') {
      const res = await apiFetch(`/api/voice/history/${encodeURIComponent(id)}/reprocess`, {
        method: 'POST',
        body: JSON.stringify({ mode: t.dataset.mode, deliver: false }),
      });
      const entry = voiceHistoryEntries.find((x) => String(x.id) === id);
      if (entry && res) {
        entry.text = typeof res.text === 'string' ? res.text : entry.text;
        entry.mode = t.dataset.mode;
      }
      renderVoiceHistory();
      toast(res && res.warning ? res.warning : 're-ran');
    } else if (action === 'entry-delete') {
      const key = 'entry:' + id;
      if (!voiceArmed.has(key)) {
        voiceArmed.add(key);
        t.classList.add('armed');
        t.textContent = 'Sure?';
        return;
      }
      voiceArmed.delete(key);
      await apiFetch(`/api/voice/history/${encodeURIComponent(id)}`, { method: 'DELETE' });
      voiceHistoryEntries = voiceHistoryEntries.filter((x) => String(x.id) !== id);
      voiceHistoryExpandedId = null;
      renderVoiceHistory();
      loadVoiceStats();
    }
  } catch (err) { toast(friendlyError(err)); }
});

voiceSearchInput.addEventListener('input', () => {
  renderVoiceHistory(); // instant client-side narrowing
  if (voiceSearchTimer) clearTimeout(voiceSearchTimer);
  voiceSearchTimer = setTimeout(() => { voiceSearchTimer = null; loadVoiceHistory(); }, 300);
});

voiceHistoryClearBtn.addEventListener('click', async () => {
  const ok = await showConfirm('Delete every saved dictation?');
  if (!ok) return;
  try {
    await apiFetch('/api/voice/history', { method: 'DELETE' });
    voiceHistoryEntries = [];
    voiceHistoryExpandedId = null;
    renderVoiceHistory();
    loadVoiceStats();
    toast('history cleared');
  } catch (_) { /* toasted */ }
});

function enterVoiceHistoryMode() {
  voiceArmed.clear();
  voiceHistoryExpandedId = null;
  loadVoiceSettings(false); // modes power the "re-run with" chips
  loadVoiceStats();
  loadVoiceHistory();
}

// ---- Mode switcher --------------------------------------------------------

const modeBridgeBtn = document.getElementById('mode-bridge');
const modePttBtn = document.getElementById('mode-ptt');
const modeFilesBtn = document.getElementById('mode-files');
const modeDictateBtn = document.getElementById('mode-dictate');
const modeHistoryBtn = document.getElementById('mode-history');
const modeSettingsBtn = document.getElementById('mode-settings');
const modeMoreBtn = document.getElementById('mode-more');
const moreOverlay = document.getElementById('more-overlay');
const viewBridge = document.getElementById('view-bridge');
const viewPtt = document.getElementById('view-ptt');
const viewFiles = document.getElementById('view-files');
const viewDictate = document.getElementById('view-dictate');

// PTT and Dictate share all their machinery and differ only in what the
// server does with the audio, so they're one mode internally.
const TALK_MODES = ['ptt', 'dictate'];

const MODE_KEY = 'iphone-bridge-mode';

// Set true when entering PTT mode without an active user gesture (e.g. via
// localStorage restore on page load). Resolved on the next pointer event so
// the iOS mic-permission prompt fires immediately on first interaction.
let pendingAutoActivate = false;

// True when the page was opened via #dictate and should start recording as
// soon as the mic is available.
let autoStartPending = false;

async function setMode(mode) {
  // Tear down whichever mode we're leaving. Each branch below only touches
  // resources for the mode being entered/left -- Bridge/PTT teardown logic
  // is untouched from before Files existed.
  if (!TALK_MODES.includes(mode) && pttModeActive) {
    pttModeActive = false;
    pttWsBackoff.reset();
    pendingKeys.length = 0;
    if (pttTransmitting && pttWs && pttWs.readyState === WebSocket.OPEN) {
      try { pttWs.send(talkProtocol + ':stop'); } catch (_) {}
    }
    teardownPtt();
    setPttUi('Activate', 'tap to activate', null);
  }
  if (mode !== 'bridge') {
    if (audioCtx && audioCtx.state !== 'closed') await stopAudio();
    if (micCtx && micCtx.state !== 'closed') await stopMic();
  }
  if (mode !== 'files' && filesTabActive) {
    exitFilesMode();
  }

  viewBridge.hidden = mode !== 'bridge';
  viewPtt.hidden = mode !== 'ptt';
  viewFiles.hidden = mode !== 'files';
  viewDictate.hidden = mode !== 'dictate';
  viewSettings.hidden = mode !== 'settings';
  viewHistory.hidden = mode !== 'history';
  modeBridgeBtn.classList.toggle('active', mode === 'bridge');
  modePttBtn.classList.toggle('active', mode === 'ptt');
  modeFilesBtn.classList.toggle('active', mode === 'files');
  modeDictateBtn.classList.toggle('active', mode === 'dictate');
  // History and Settings live inside the More sheet, so the More lamp stands in
  // for them on the faceplate -- otherwise those modes would light nothing.
  modeMoreBtn.classList.toggle('active', mode === 'settings' || mode === 'history');

  if (TALK_MODES.includes(mode)) {
    // Decides whether the server routes audio to the virtual cable or keeps
    // it for whisper. Must be set before any start/stop frame goes out.
    talkProtocol = mode;
  }

  if (TALK_MODES.includes(mode)) {
    pttModeActive = true;
    // Open the control WebSocket immediately so nav keys (arrows, Esc,
    // Enter, Ctrl-X shortcuts, tab/desktop switching) work without the user
    // having to tap Activate first.
    openPttWs();
    // Auto-trigger Activate. If we got here from a user gesture (the mode
    // pill being tapped), iOS allows the mic-permission prompt to appear
    // immediately. If we got here from localStorage restore on page load
    // (no gesture), defer to the first pointer event.
    if (!pttActivated) {
      activatePtt()
        .then(() => { if (mode === 'dictate') beginDictation(); })
        .catch(() => { pendingAutoActivate = true; });
    } else if (mode === 'dictate') {
      beginDictation();
    }
    // Mode chips need the settings document; cached after the first fetch.
    if (mode === 'dictate') loadVoiceSettings(false);
  } else if (mode === 'files') {
    enterFilesMode();
  } else if (mode === 'settings') {
    loadVoiceSettings(true);
  } else if (mode === 'history') {
    enterVoiceHistoryMode();
  }

  try { localStorage.setItem(MODE_KEY, mode); } catch (_) { /* private mode */ }
}

modeBridgeBtn.addEventListener('click', () => setMode('bridge'));
modePttBtn.addEventListener('click', () => setMode('ptt'));
modeFilesBtn.addEventListener('click', () => setMode('files'));
modeDictateBtn.addEventListener('click', () => setMode('dictate'));
// History/Settings live in the More sheet: pick one, the sheet closes behind it.
modeHistoryBtn.addEventListener('click', () => { closeMoreSheet(); setMode('history'); });
modeSettingsBtn.addEventListener('click', () => { closeMoreSheet(); setMode('settings'); });

function openMoreSheet() { moreOverlay.hidden = false; }
function closeMoreSheet() { moreOverlay.hidden = true; }
modeMoreBtn.addEventListener('click', openMoreSheet);
document.getElementById('more-cancel').addEventListener('click', closeMoreSheet);
// Tapping the dimmed backdrop (but not the card) dismisses, matching the other sheets.
moreOverlay.addEventListener('click', (e) => { if (e.target === moreOverlay) closeMoreSheet(); });

// Manual reload -- when the page is a home-screen web clip there's no Safari
// chrome to pull-to-refresh, so this is the only way out of a wedged state.
document.getElementById('refresh-btn').addEventListener('click', () => {
  location.reload();
});

// If PTT mode was restored on load without a user gesture, the initial
// activatePtt() call will have been rejected by iOS. Try again on the very
// first pointer interaction (which IS a gesture).
document.addEventListener('pointerdown', () => {
  if (pendingAutoActivate && !pttActivated && (!viewPtt.hidden || !viewDictate.hidden)) {
    pendingAutoActivate = false;
    activatePtt().then(() => { if (!viewDictate.hidden) beginDictation(); }).catch(() => {});
  }
}, { capture: true });

// ---- iOS lifecycle recovery ----------------------------------------------
// iOS aggressively suspends WebSockets, AudioContexts, and MediaStream tracks
// when the home-screen web clip backgrounds (tab switch, lock, app switcher).
// Proactively heal on every "we're visible again" signal so the user doesn't
// have to tap Activate after coming back.

function recoverPtt() {
  if (!pttModeActive || (viewPtt.hidden && viewDictate.hidden)) return;
  // Wake the audio context if iOS suspended it.
  if (pttCtx && pttCtx.state === 'suspended') {
    pttCtx.resume().catch(() => {});
  }
  // Re-open the control socket immediately (don't wait for the backoff timer).
  if (!pttWs || pttWs.readyState === WebSocket.CLOSED) {
    pttWsBackoff.reset();
    openPttWs();
  }
}

// Same idea for Bridge mode's two sockets -- only acts on connections the
// user actually intends to have open (audioIntentOn / micIntentOn), so this
// is a no-op if Bridge is idle or the user deliberately stopped one side.
function recoverBridge() {
  if (viewBridge.hidden) return;
  if (audioCtx && audioCtx.state === 'suspended') {
    audioCtx.resume().catch(() => {});
  }
  if (audioIntentOn && (!ws || ws.readyState === WebSocket.CLOSED)) {
    audioWsBackoff.reset();
    openAudioWs();
  }
  if (micCtx && micCtx.state === 'suspended') {
    micCtx.resume().catch(() => {});
  }
  if (micIntentOn && (!micWs || micWs.readyState === WebSocket.CLOSED)) {
    micWsBackoff.reset();
    openMicWs();
  }
}

// Files tab has no persistent connection to heal -- just refresh the
// sessions panel so uptimes/alive-state aren't stale after backgrounding.
function recoverFiles() {
  if (!filesTabActive || viewFiles.hidden) return;
  renderSessions();
}

document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'visible') { recoverPtt(); recoverBridge(); recoverFiles(); }
});

// bfcache restore (rare on iOS standalone clips, but covers full-Safari case).
window.addEventListener('pageshow', (e) => { if (e.persisted) { recoverPtt(); recoverBridge(); recoverFiles(); } });

// Network came back -- usually paired with visibilitychange, but not always.
window.addEventListener('online', () => { recoverPtt(); recoverBridge(); recoverFiles(); });

// Restore last-used mode on load -- unless the URL asks for one, which is how
// the iPhone Action Button shortcut lands straight on dictation.
const HASH_MODES = {
  '#dictate': 'dictate', '#ptt': 'ptt', '#files': 'files', '#bridge': 'bridge',
  '#settings': 'settings', '#history': 'history',
};
try {
  const fromHash = HASH_MODES[location.hash];
  if (fromHash) {
    setMode(fromHash);
    // Arriving via the shortcut means "start now" -- the press that opened
    // the app was on the phone's button, not on ours.
    if (fromHash === 'dictate') autoStartDictation();
  } else {
    const saved = localStorage.getItem(MODE_KEY);
    if (['ptt', 'files', 'dictate', 'settings', 'history'].includes(saved)) setMode(saved);
  }
} catch (_) { /* ignore */ }

// Opening #dictate should begin recording without a second tap. Activation is
// owned by setMode -- requesting the mic twice at once leaves getUserMedia
// racing itself and the press reducer out of sync with reality, so this only
// ever records the intent and lets the existing activation path fulfil it.
function autoStartDictation() {
  autoStartPending = true;
  if (pttActivated) beginDictation();
}

function beginDictation() {
  if (!autoStartPending || pttTransmitting) return;
  autoStartPending = false;
  pttPressState = { ...pttPressState, activated: true, transmitting: false };
  startTransmitting();
}
