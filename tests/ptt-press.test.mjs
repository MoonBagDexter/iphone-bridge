// Plain-Node test for the pure PTT press/release state machine in web/app.js.
// Run: node tests/ptt-press.test.mjs
//
// Loads app.js as text and evaluates ONLY the code between the
// `ptt-press-machine` markers, so this test exercises the exact reducer
// shipped to the browser without needing a DOM or bundler.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import vm from 'node:vm';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const appJsPath = path.join(__dirname, '..', 'web', 'app.js');
const src = readFileSync(appJsPath, 'utf8');

const START_MARKER = '// --- ptt-press-machine (pure; tested by tests/ptt-press.test.mjs) ---';
const END_MARKER = '// --- end ptt-press-machine ---';

const startIdx = src.indexOf(START_MARKER);
const endIdx = src.indexOf(END_MARKER);
if (startIdx === -1 || endIdx === -1 || endIdx <= startIdx) {
  throw new Error('Could not locate ptt-press-machine markers in web/app.js');
}
const machineSrc = src.slice(startIdx, endIdx);

// Need PTT_HOLD_MS in scope since the reducer references it as a free
// variable (defined just above the markers in app.js).
const HOLD_MARKER_RE = /const PTT_HOLD_MS = (\d+);/;
const holdMatch = src.match(HOLD_MARKER_RE);
if (!holdMatch) throw new Error('Could not find PTT_HOLD_MS in web/app.js');

const sandbox = {};
vm.createContext(sandbox);
vm.runInContext(`const PTT_HOLD_MS = ${holdMatch[1]};\n${machineSrc}\nthis.pttPressReduce = pttPressReduce;`, sandbox);
const { pttPressReduce } = sandbox;

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

test('a lost pointerup does not deadlock the button forever', () => {
  // iOS steals gestures (control centre swipe, call banner, Slide-Over), and
  // the pointerup for the stolen press either never arrives or arrives with a
  // different id. Before this fix, activePointerId stayed set and every later
  // press was discarded as a duplicate -- the button was dead until reload.
  let state = { ...freshActivatedState(), activePointerId: 7 };

  // A genuinely new press, different pointer, while the stale one is "active".
  const down = pttPressReduce(state, { type: 'down', pointerId: 8 }, 1000);
  assertEqual(down.actions, ['startTx'], 'new press must be honoured, not swallowed');
  assertEqual(down.state.activePointerId, 8, 'the new pointer takes ownership');

  // And it must still toggle off on the next tap.
  const up = pttPressReduce(down.state, { type: 'up', pointerId: 8 }, 1100);
  const off = pttPressReduce(up.state, { type: 'down', pointerId: 9 }, 2000);
  assertEqual(off.actions, ['stopTx'], 'second tap must stop it');
});

test('a true duplicate pointerdown (same id) is still ignored', () => {
  // The synthetic double-down iOS emits for one physical touch carries the
  // same pointerId, which is what distinguishes it from a real second press.
  const state = { ...freshActivatedState(), activePointerId: 3, transmitting: true };
  const dup = pttPressReduce(state, { type: 'down', pointerId: 3 }, 1000);
  assertEqual(dup.actions, [], 'duplicate must not toggle');
  assertEqual(dup.state.activePointerId, 3, 'state unchanged');
});

function freshActivatedState() {
  return {
    activated: true,
    transmitting: false,
    pressOpenedTx: false,
    pressTime: 0,
    activePointerId: null,
  };
}

// ---- tests ------------------------------------------------------------------

test('not-yet-activated: first down just activates', () => {
  const state = {
    activated: false,
    transmitting: false,
    pressOpenedTx: false,
    pressTime: 0,
    activePointerId: null,
  };
  const { state: s1, actions } = pttPressReduce(state, { type: 'down', pointerId: 1 }, 1000);
  assertEqual(actions, ['activate'], 'expected activate action');
  assertEqual(s1.transmitting, false, 'should not start transmitting while unactivated');
  assertEqual(s1.activePointerId, 1, 'press should still be tracked as active');
});

test('first tap -> startTx, stays on after up (<250ms)', () => {
  let state = freshActivatedState();

  let r = pttPressReduce(state, { type: 'down', pointerId: 1 }, 1000);
  state = r.state;
  assertEqual(r.actions, ['startTx'], 'down while idle should startTx');
  assertEqual(state.transmitting, true, 'should be transmitting after down');

  r = pttPressReduce(state, { type: 'up', pointerId: 1 }, 1100); // 100ms later
  state = r.state;
  assertEqual(r.actions, [], 'quick release should not stop transmission');
  assertEqual(state.transmitting, true, 'should remain transmitting after quick tap release');
});

test('quick second tap 150ms later -> stopTx (the bug case)', () => {
  let state = freshActivatedState();

  // First tap: down then up within hold window -> stays on.
  let r = pttPressReduce(state, { type: 'down', pointerId: 1 }, 0);
  state = r.state;
  r = pttPressReduce(state, { type: 'up', pointerId: 1 }, 100);
  state = r.state;
  assertEqual(state.transmitting, true, 'sanity: still transmitting after first tap');

  // Second tap lands 150ms after the FIRST pointerdown's timestamp (i.e. only
  // 50ms after the first tap's release) -- this is exactly the case the old
  // `Date.now() - pttPressTime < 200` guard incorrectly swallowed.
  r = pttPressReduce(state, { type: 'down', pointerId: 2 }, 150);
  state = r.state;
  assertEqual(r.actions, ['stopTx'], 'second tap must toggle off, even if it lands <200ms after the first pointerdown');
  assertEqual(state.transmitting, false, 'should be off after second tap down');

  r = pttPressReduce(state, { type: 'up', pointerId: 2 }, 220);
  state = r.state;
  assertEqual(r.actions, [], 'release of the toggle-off tap should not double-fire stopTx');
});

test('hold 400ms -> startTx on down, stopTx on release', () => {
  let state = freshActivatedState();

  let r = pttPressReduce(state, { type: 'down', pointerId: 1 }, 0);
  state = r.state;
  assertEqual(r.actions, ['startTx'], 'down should startTx');

  r = pttPressReduce(state, { type: 'up', pointerId: 1 }, 400);
  state = r.state;
  assertEqual(r.actions, ['stopTx'], 'release after >=250ms hold should stopTx');
  assertEqual(state.transmitting, false, 'should be off after a real hold-release');
});

test('duplicate synthetic pointerdown during an active press (same pointerId, no up between) -> ignored', () => {
  let state = freshActivatedState();

  let r = pttPressReduce(state, { type: 'down', pointerId: 1 }, 0);
  state = r.state;
  assertEqual(r.actions, ['startTx'], 'first down should startTx');

  // iOS fires a synthetic duplicate pointerdown microseconds later, same pointerId.
  r = pttPressReduce(state, { type: 'down', pointerId: 1 }, 1);
  state = r.state;
  assertEqual(r.actions, [], 'duplicate pointerdown with no up between must be ignored');
  assertEqual(state.transmitting, true, 'transmission state must be unaffected by the duplicate');

  // Real release should still work normally afterward.
  r = pttPressReduce(state, { type: 'up', pointerId: 1 }, 400);
  state = r.state;
  assertEqual(r.actions, ['stopTx'], 'release after the duplicate should still behave like a normal hold-release');
});

test('pointercancel behaves like up', () => {
  let state = freshActivatedState();

  let r = pttPressReduce(state, { type: 'down', pointerId: 1 }, 0);
  state = r.state;

  r = pttPressReduce(state, { type: 'cancel', pointerId: 1 }, 400);
  state = r.state;
  assertEqual(r.actions, ['stopTx'], 'pointercancel after a hold should stopTx just like pointerup');
  assertEqual(state.activePointerId, null, 'pointercancel should clear the active press like pointerup does');
});

test('up from a stale/foreign pointerId (no matching active press) is ignored', () => {
  let state = freshActivatedState();
  const r = pttPressReduce(state, { type: 'up', pointerId: 99 }, 1000);
  assertEqual(r.actions, [], 'an up with no matching active press must be a no-op');
  assertEqual(r.state, state, 'state must be unchanged');
});

// ---- summary ----------------------------------------------------------------

console.log(`\n${passCount} passed, ${failCount} failed`);
if (failCount > 0) {
  process.exit(1);
}
