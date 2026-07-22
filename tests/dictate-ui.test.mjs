// Plain-Node test for the pure dictation status helpers in web/app.js.
// Run: node tests/dictate-ui.test.mjs
//
// Loads app.js as text and evaluates ONLY the code between the
// `dictate-ui-pure` markers, so this exercises the exact logic shipped to the
// browser without needing a DOM or bundler.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import vm from 'node:vm';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const appJsPath = path.join(__dirname, '..', 'web', 'app.js');
const src = readFileSync(appJsPath, 'utf8');

const START_MARKER = '// --- dictate-ui-pure (pure; tested by tests/dictate-ui.test.mjs) ---';
const END_MARKER = '// --- end dictate-ui-pure ---';

const startIdx = src.indexOf(START_MARKER);
const endIdx = src.indexOf(END_MARKER);
if (startIdx === -1 || endIdx === -1 || endIdx <= startIdx) {
  throw new Error('Could not locate dictate-ui-pure markers in web/app.js');
}

const sandbox = {};
vm.createContext(sandbox);
vm.runInContext(
  `${src.slice(startIdx, endIdx)}\nthis.dictateView = dictateView;\nthis.dictateDuration = dictateDuration;`,
  sandbox,
);
const { dictateView, dictateDuration } = sandbox;

// ---- tiny test harness -----------------------------------------------------

let passCount = 0;
let failCount = 0;
const failures = [];

function test(name, fn) {
  try {
    fn();
    passCount++;
    console.log(`PASS: ${name}`);
  } catch (e) {
    failCount++;
    failures.push({ name, error: e });
    console.log(`FAIL: ${name}`);
    console.log(`      ${e.message}`);
  }
}

function assertEqual(actual, expected, msg) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) {
    throw new Error(`${msg || 'assertion failed'}: expected ${e}, got ${a}`);
  }
}

// ---- duration phrasing ------------------------------------------------------

test('duration: sub-minute reads in seconds', () => {
  assertEqual(dictateDuration(4.25), '4.3s of audio');
});

test('duration: exactly a minute drops the seconds', () => {
  assertEqual(dictateDuration(60), '1m of audio');
});

test('duration: minutes and seconds together', () => {
  assertEqual(dictateDuration(95), '1m 35s of audio');
});

test('duration: zero-length recording says so rather than "0.0s"', () => {
  assertEqual(dictateDuration(0), 'no audio captured');
});

test('duration: missing or nonsense values degrade gracefully', () => {
  assertEqual(dictateDuration(undefined), 'no audio captured');
  assertEqual(dictateDuration(NaN), 'no audio captured');
  assertEqual(dictateDuration(-3), 'no audio captured');
  assertEqual(dictateDuration('12'), 'no audio captured');
});

// ---- frame -> button face ---------------------------------------------------

test('ignores frames belonging to other features', () => {
  assertEqual(dictateView({ type: 'format', sampleRate: 48000 }), null);
  assertEqual(dictateView(null), null);
  assertEqual(dictateView({ type: 'dictation', state: 'who knows' }), null);
});

test('recording: button reads as live and says how to stop', () => {
  assertEqual(dictateView({ type: 'dictation', state: 'recording' }), {
    label: 'Listening…', state: 'tap to stop', cls: 'on', text: null,
  });
});

test('transcribing: shows how much audio is being processed', () => {
  assertEqual(dictateView({ type: 'dictation', state: 'transcribing', seconds: 12.5 }), {
    label: 'Transcribing…', state: '12.5s of audio', cls: 'draining', text: null,
  });
});

test('done: confirms the text was delivered', () => {
  assertEqual(dictateView({ type: 'dictation', state: 'done', text: 'hello world' }), {
    label: 'Dictate', state: 'typed into your PC', cls: null, text: 'hello world',
  });
});

test('done: silence reports nothing heard rather than claiming success', () => {
  assertEqual(dictateView({ type: 'dictation', state: 'done', text: '' }), {
    label: 'Dictate', state: 'nothing heard', cls: null, text: '',
  });
});

test('done: hitting the cap is surfaced, not silently truncated', () => {
  assertEqual(dictateView({ type: 'dictation', state: 'done', text: 'a lot', overflowed: true }), {
    label: 'Dictate', state: 'hit the 10 minute limit', cls: null, text: 'a lot',
  });
});

test('done: a missing text field is treated as empty, not undefined', () => {
  assertEqual(dictateView({ type: 'dictation', state: 'done' }), {
    label: 'Dictate', state: 'nothing heard', cls: null, text: '',
  });
});

test('error: surfaces the server message', () => {
  assertEqual(dictateView({ type: 'dictation', state: 'error', error: 'model not found' }), {
    label: 'Dictate', state: 'model not found', cls: null, text: null,
  });
});

test('error: falls back to a readable message when none is given', () => {
  assertEqual(dictateView({ type: 'dictation', state: 'error' }), {
    label: 'Dictate', state: 'transcription failed', cls: null, text: null,
  });
});

// ---- summary ----------------------------------------------------------------

console.log(`\n${passCount} passed, ${failCount} failed`);
if (failCount > 0) {
  for (const { name, error } of failures) {
    console.log(`\n--- ${name} ---`);
    console.log(error.stack || error.message);
  }
  process.exit(1);
}
