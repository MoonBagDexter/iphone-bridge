// AudioWorklet ring-buffer playback.
//
// Decouples WebSocket arrival timing from audio-thread playback timing.
// Main thread receives PCM frames over WS and posts them here. The processor
// drains 128-sample quanta from the ring buffer at the audio thread's natural
// rate. Bursty arrivals are absorbed by the ring buffer; smooth output.

const SAMPLE_RATE = 48000;
const CHANNELS = 2;
const RING_CAPACITY = SAMPLE_RATE * CHANNELS * 2;   // 2 seconds of stereo
const CUSHION_TARGET_SAMPLES = (SAMPLE_RATE / 20) * CHANNELS; // 50ms stereo
const MAX_CUSHION_SAMPLES = (SAMPLE_RATE / 4) * CHANNELS;     // 250ms -- catch up sooner
const STATS_EVERY = SAMPLE_RATE / 10;               // ~10 reports/sec

class StreamPlayer extends AudioWorkletProcessor {
  constructor() {
    super();
    this.buf = new Float32Array(RING_CAPACITY);
    this.read = 0;
    this.write = 0;
    this.fill = 0;          // interleaved sample count in buffer
    this.started = false;   // hold output silent until cushion fills
    this.underrunsThisReport = 0;
    this.lastStatsFrame = 0;

    this.port.onmessage = (e) => {
      const d = e.data;
      if (d.type === 'pcm') {
        this.push(d.samples);
      } else if (d.type === 'reset') {
        this.read = this.write = this.fill = 0;
        this.started = false;
      }
    };
  }

  push(samples) {
    const n = samples.length;
    if (n === 0) return;

    // If we'd exceed our overflow ceiling, drop the oldest -- we got behind,
    // catching up is preferable to hoarding latency.
    if (this.fill + n > MAX_CUSHION_SAMPLES) {
      const drop = Math.min(this.fill, this.fill + n - MAX_CUSHION_SAMPLES);
      this.read = (this.read + drop) % RING_CAPACITY;
      this.fill -= drop;
    }

    // Append, wrapping at ring boundary.
    const first = Math.min(n, RING_CAPACITY - this.write);
    this.buf.set(samples.subarray(0, first), this.write);
    if (n > first) {
      this.buf.set(samples.subarray(first), 0);
    }
    this.write = (this.write + n) % RING_CAPACITY;
    this.fill += n;
  }

  process(_inputs, outputs) {
    const out = outputs[0];
    const L = out[0];
    const R = out[1];
    const frames = L.length;

    if (!this.started) {
      if (this.fill >= CUSHION_TARGET_SAMPLES) {
        this.started = true;
      } else {
        L.fill(0);
        R.fill(0);
        return true;
      }
    }

    for (let i = 0; i < frames; i++) {
      if (this.fill >= CHANNELS) {
        L[i] = this.buf[this.read];
        this.read = (this.read + 1) % RING_CAPACITY;
        R[i] = this.buf[this.read];
        this.read = (this.read + 1) % RING_CAPACITY;
        this.fill -= CHANNELS;
      } else {
        L[i] = 0;
        R[i] = 0;
        this.underrunsThisReport++;
      }
    }

    // If we've been silent for >250ms straight (entire frame run with no data),
    // re-arm the cushion so the next data burst won't immediately glitch.
    if (this.fill === 0 && this.underrunsThisReport > frames * 100) {
      this.started = false;
      this.underrunsThisReport = 0;
    }

    if (currentFrame - this.lastStatsFrame >= STATS_EVERY) {
      this.port.postMessage({
        type: 'stats',
        cushionMs: Math.round(this.fill / CHANNELS / SAMPLE_RATE * 1000),
        underruns: this.underrunsThisReport,
      });
      this.underrunsThisReport = 0;
      this.lastStatsFrame = currentFrame;
    }

    return true;
  }
}

registerProcessor('stream-player', StreamPlayer);
