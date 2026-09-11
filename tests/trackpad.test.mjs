import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';

const context = vm.createContext({});
vm.runInContext(readFileSync(new URL('../web/trackpad.js', import.meta.url), 'utf8') + '\nthis.Gestures = TrackpadGestures;', context);
function setup() {
  const messages = [];
  return { pad: new context.Gestures(m => messages.push(JSON.parse(JSON.stringify(m)))), messages };
}

test('a short tap clicks once; holding still does not accidentally click', () => {
  const { pad, messages } = setup();
  pad.down(1, 20, 20, 0); pad.up(1, 100);
  assert.deepEqual(messages, [{ op: 'click' }]);
  pad.down(2, 20, 20, 1000); pad.up(2, 2000);
  assert.equal(messages.length, 1);
});
test('slow strokes are precise and faster strokes cover more ground', () => {
  const slow = setup(), fast = setup();
  for (const { pad } of [slow, fast]) pad.down(1, 0, 0, 0);
  slow.pad.move(1, 10, 0, 100); fast.pad.move(1, 10, 0, 10);
  assert.ok(slow.messages[0].x < fast.messages[0].x);
  slow.pad.up(1, 110); fast.pad.up(1, 110);
  assert.ok(!slow.messages.some(m => m.op === 'click'));
});
test('two-finger tap right-clicks exactly once', () => {
  const { pad, messages } = setup();
  pad.down(1, 10, 10, 0); pad.down(2, 40, 10, 20);
  pad.up(1, 100); pad.up(2, 120);
  assert.deepEqual(messages, [{ op: 'right' }]);
});
test('two-finger scrolling never turns into movement or click when one finger lifts', () => {
  const { pad, messages } = setup();
  pad.down(1, 0, 0, 0); pad.down(2, 30, 0, 10);
  pad.move(1, 0, 30, 40); pad.move(2, 30, 30, 45);
  pad.up(1, 70); pad.move(2, 80, 80, 90); pad.up(2, 110);
  assert.ok(messages.length > 0);
  assert.ok(messages.every(m => m.op === 'wheel'));
  assert.ok(messages.every(m => m.y > 0));
});
test('tap then hold drags, releasing on lift', () => {
  const { pad, messages } = setup();
  pad.down(1, 20, 20, 0); pad.up(1, 70);
  pad.down(2, 20, 20, 180); pad.move(2, 60, 60, 220); pad.up(2, 600);
  assert.deepEqual(messages.map(m => m.op), ['click', 'down', 'move', 'up']);
});
test('cancellation releases a held button and forgets the previous tap', () => {
  const { pad, messages } = setup();
  pad.down(1, 0, 0, 0); pad.up(1, 50); pad.down(2, 0, 0, 100);
  pad.reset(); pad.up(2, 110); pad.down(3, 0, 0, 120); pad.up(3, 160);
  assert.deepEqual(messages.map(m => m.op), ['click', 'down', 'up', 'click']);
});
test('adding a third finger cancels tap recognition until every finger lifts', () => {
  const { pad, messages } = setup();
  pad.down(1, 0, 0, 0); pad.down(2, 20, 0, 10); pad.down(3, 40, 0, 20);
  pad.up(3, 40); pad.up(2, 50); pad.up(1, 60);
  assert.deepEqual(messages, []);
});
test('lifting and repositioning a finger does not jump the cursor', () => {
  const { pad, messages } = setup();
  pad.down(1, 0, 0, 0); pad.move(1, 30, 0, 100); pad.up(1, 110);
  pad.down(2, 250, 300, 400); pad.move(2, 251, 300, 420);
  assert.ok(messages.at(-1).x < 2);
  assert.equal(messages.at(-1).y, 0);
});
