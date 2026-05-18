// Mic capture worklet -- reads input PCM (whatever iOS gives us, typically mono
// from AirPods mic) and pushes interleaved-stereo Float32 chunks to the main
// thread for WS upload to the bridge.

class MicCapture extends AudioWorkletProcessor {
  process(inputs) {
    const input = inputs[0];
    if (!input || input.length === 0) return true;
    const ch = input.length;
    const frames = input[0].length;
    if (frames === 0) return true;

    // Always emit stereo so the Windows side can use a single fixed format.
    const out = new Float32Array(frames * 2);
    if (ch >= 2) {
      const L = input[0];
      const R = input[1];
      for (let i = 0; i < frames; i++) {
        out[i * 2] = L[i];
        out[i * 2 + 1] = R[i];
      }
    } else {
      const M = input[0];
      for (let i = 0; i < frames; i++) {
        const s = M[i];
        out[i * 2] = s;
        out[i * 2 + 1] = s;
      }
    }
    this.port.postMessage({ type: 'pcm', samples: out }, [out.buffer]);
    return true;
  }
}

registerProcessor('mic-capture', MicCapture);
