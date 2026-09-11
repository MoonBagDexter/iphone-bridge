// Gesture geometry is independent of the DOM and transport so it can be replayed in tests.
class TrackpadGestures {
  constructor(emit) { this.emit = emit; this.reset(); }
  reset() {
    if (this.dragging) this.emit({ op: 'up' });
    this.points = new Map(); this.dragging = false; this.lastTap = null;
    this.maxFingers = 0; this.moved = false; this.suppressed = false;
  }
  center() {
    const points = [...this.points.values()];
    return { x: points.reduce((n, p) => n + p.x, 0) / points.length,
      y: points.reduce((n, p) => n + p.y, 0) / points.length };
  }
  down(id, x, y, now) {
    if (this.points.has(id)) return;
    if (!this.points.size) {
      this.started = now; this.moved = false; this.suppressed = false; this.maxFingers = 0;
      if (this.lastTap && now - this.lastTap.at < 300 && Math.hypot(x - this.lastTap.x, y - this.lastTap.y) < 35) {
        this.dragging = true; this.emit({ op: 'down' });
      }
      this.lastTap = null;
    }
    this.points.set(id, { x, y, startX: x, startY: y });
    this.maxFingers = Math.max(this.maxFingers, this.points.size);
    if (this.points.size > 1 && this.dragging) {
      this.emit({ op: 'up' }); this.dragging = false; this.moved = true;
    }
    if (this.points.size > 2) this.suppressed = true;
    this.previous = this.center(); this.time = now;
  }
  move(id, x, y, now) {
    const p = this.points.get(id);
    if (!p) return;
    this.points.set(id, { ...p, x, y });
    if (Math.hypot(x - p.startX, y - p.startY) > 7) this.moved = true;
    const center = this.center();
    const dx = center.x - this.previous.x, dy = center.y - this.previous.y;
    const dt = Math.max(8, now - this.time);
    this.previous = center; this.time = now;
    if (this.suppressed) return;
    if (this.points.size === 2) {
      // Windows accepts partial wheel deltas; preserve gentle two-finger scrolling.
      if (this.moved) this.emit({ op: 'wheel', x: dx * 2.5, y: dy * 2.5 });
    } else if (this.maxFingers === 1) {
      const speed = Math.hypot(dx, dy) / dt;
      const gain = 0.8 + Math.min(2.4, speed * 1.6);
      this.emit({ op: 'move', x: dx * gain, y: dy * gain });
    }
  }
  up(id, now) {
    const p = this.points.get(id);
    if (!p) return;
    this.points.delete(id);
    if (this.points.size) {
      // Do not turn the last finger of a scroll into cursor motion or a click.
      this.previous = this.center(); this.time = now;
      return;
    }
    if (this.dragging) { this.emit({ op: 'up' }); this.dragging = false; return; }
    if (!this.suppressed && !this.moved && now - this.started < 250) {
      this.emit({ op: this.maxFingers === 2 ? 'right' : 'click' });
      if (this.maxFingers === 1) this.lastTap = { x: p.x, y: p.y, at: now };
    }
  }
}

function mountTrackpad(surface, status) {
  let socket = null, active = false, ready = false, retry = null, frame = null;
  let pending = null;
  let remainder = { move: { x: 0, y: 0 }, wheel: { x: 0, y: 0 } };
  const write = (message) => {
    if (!ready || socket?.readyState !== WebSocket.OPEN) return false;
    // Never replay pointer motion after a stalled network link.
    if (socket.bufferedAmount > 4096) { socket.close(); return false; }
    socket.send(JSON.stringify(message)); return true;
  };
  const flush = () => {
    if (frame !== null) cancelAnimationFrame(frame);
    frame = null;
    if (!pending) return;
    const { op } = pending, rest = remainder[op];
    const x = Math.trunc(pending.x + rest.x), y = Math.trunc(pending.y + rest.y);
    rest.x += pending.x - x; rest.y += pending.y - y;
    pending = null;
    // Preserve fractional pixels, but reject an implausibly large accumulated jump.
    if ((x || y) && Math.abs(x) <= 2048 && Math.abs(y) <= 2048) write({ op, x, y });
  };
  const gestures = new TrackpadGestures((message) => {
    if (message.op === 'move' || message.op === 'wheel') {
      if (pending && pending.op !== message.op) flush();
      pending ||= { op: message.op, x: 0, y: 0 };
      pending.x += message.x; pending.y += message.y;
      if (frame === null) frame = requestAnimationFrame(flush);
    } else { flush(); write(message); }
    surface.classList.toggle('dragging', message.op === 'down' || (gestures.dragging && message.op !== 'up'));
  });
  function clearGesture() {
    if (frame !== null) cancelAnimationFrame(frame);
    frame = null; pending = null;
    remainder = { move: { x: 0, y: 0 }, wheel: { x: 0, y: 0 } };
    gestures.reset(); surface.classList.remove('touching', 'dragging');
  }
  function connect() {
    if (!active || document.hidden || socket) return;
    status.textContent = 'Connecting…';
    const ws = new WebSocket(`${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}/trackpad`);
    socket = ws;
    ws.onmessage = (event) => {
      let data; try { data = JSON.parse(event.data); } catch (_) { return; }
      if (data.type === 'ready') { ready = true; status.textContent = 'Connected'; }
      if (data.type === 'busy') status.textContent = 'Trackpad is open on another device';
    };
    ws.onclose = () => {
      if (socket !== ws) return;
      ready = false; socket = null; clearGesture();
      if (active && !document.hidden) {
        if (!status.textContent.includes('another device')) status.textContent = 'Reconnecting…';
        retry = setTimeout(connect, 1200);
      }
    };
    ws.onerror = () => {}; // onclose owns recovery.
  }
  function disconnect() {
    clearTimeout(retry); clearGesture(); ready = false;
    const old = socket; socket = null; old?.close();
  }
  surface.addEventListener('pointerdown', (e) => {
    if (e.pointerType === 'mouse' && e.button !== 0) return;
    e.preventDefault();
    if (!ready) { connect(); return; }
    surface.setPointerCapture(e.pointerId);
    gestures.down(e.pointerId, e.clientX, e.clientY, performance.now());
    surface.classList.add('touching');
  });
  surface.addEventListener('pointermove', (e) => {
    if (!ready) return;
    gestures.move(e.pointerId, e.clientX, e.clientY, performance.now());
  });
  surface.addEventListener('pointerup', (e) => {
    gestures.move(e.pointerId, e.clientX, e.clientY, performance.now());
    gestures.up(e.pointerId, performance.now());
    if (!gestures.points.size) surface.classList.remove('touching', 'dragging');
  });
  surface.addEventListener('pointercancel', clearGesture);
  surface.addEventListener('lostpointercapture', (e) => { if (gestures.points.has(e.pointerId)) clearGesture(); });
  surface.addEventListener('contextmenu', (e) => e.preventDefault());
  surface.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); write({ op: 'click' }); }
  });
  document.addEventListener('visibilitychange', () => { if (document.hidden) disconnect(); else connect(); });
  window.addEventListener('pagehide', disconnect);
  window.addEventListener('pageshow', connect);
  window.addEventListener('blur', clearGesture);
  // Keep a stationary drag alive. Server releases it within two seconds if the phone disappears.
  setInterval(() => { if (active && gestures.dragging) write({ op: 'ping' }); }, 500);
  return {
    enter() { active = true; connect(); },
    exit() { active = false; disconnect(); },
  };
}
