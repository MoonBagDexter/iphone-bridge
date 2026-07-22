// Plain-Node test for the pure voice settings/history helpers in web/app.js.
// Run: node tests/voice-ui.test.mjs
//
// Loads app.js as text and evaluates ONLY the code between the
// `voice-ui-pure` markers, so this exercises the exact logic shipped to the
// browser without needing a DOM or bundler.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import vm from 'node:vm';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const appJsPath = path.join(__dirname, '..', 'web', 'app.js');
const src = readFileSync(appJsPath, 'utf8');

const START_MARKER = '// --- voice-ui-pure (pure; tested by tests/voice-ui.test.mjs) ---';
const END_MARKER = '// --- end voice-ui-pure ---';

const startIdx = src.indexOf(START_MARKER);
const endIdx = src.indexOf(END_MARKER);
if (startIdx === -1 || endIdx === -1 || endIdx <= startIdx) {
  throw new Error('Could not locate voice-ui-pure markers in web/app.js');
}

const EXPORTS = [
  'VOICE_PROVIDERS',
  'voiceNormalizeProvider',
  'voiceProviderLabel',
  'voiceSettingsPayload',
  'voiceKeyFieldState',
  'voiceModelPlaceholder',
  'voiceAtMs',
  'voiceRelativeTime',
  'voiceClipLength',
  'voicePreview',
  'voiceHistoryRow',
  'voiceDoneView',
  'voiceFilterHistory',
  'voiceModeChips',
  'voiceStatCards',
  'voiceModeId',
];

const sandbox = {};
vm.createContext(sandbox);
vm.runInContext(
  `${src.slice(startIdx, endIdx)}\n${EXPORTS.map((n) => `this.${n} = ${n};`).join('\n')}`,
  sandbox,
);
const {
  VOICE_PROVIDERS,
  voiceNormalizeProvider,
  voiceProviderLabel,
  voiceSettingsPayload,
  voiceKeyFieldState,
  voiceModelPlaceholder,
  voiceAtMs,
  voiceRelativeTime,
  voiceClipLength,
  voicePreview,
  voiceHistoryRow,
  voiceDoneView,
  voiceFilterHistory,
  voiceModeChips,
  voiceStatCards,
  voiceModeId,
} = sandbox;

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

function assertTrue(cond, msg) {
  if (!cond) throw new Error(msg || 'expected true');
}

// A realistic GET /api/voice/settings body, keys and all.
function serverSettings() {
  return {
    modes: [
      {
        id: 'email',
        name: 'Email',
        prompt: 'Write it as an email.',
        apps: ['Mail', 'Outlook'],
        use_replacements: true,
        use_vocabulary: true,
        use_context: false,
      },
      {
        id: 'code',
        name: 'Code',
        prompt: 'Terse.',
        apps: [],
        use_replacements: false,
        use_vocabulary: true,
        use_context: true,
      },
    ],
    active_mode: 'code',
    auto_mode: true,
    replacements: [{ from: 'clod', to: 'Claude' }],
    vocabulary: ['Tailscale', 'whisper.cpp'],
    history_cap: 500,
    ai: {
      provider: 'anthropic',
      model: 'claude-sonnet-4-5',
      has_key: { anthropic: true, openai: false, gemini: false, openrouter: false },
      default_models: {
        anthropic: 'claude-haiku-4-5',
        openai: 'gpt-5-mini',
        gemini: 'gemini-2.5-flash',
        openrouter: 'auto',
      },
    },
  };
}

// ---- the settings PUT payload ----------------------------------------------

test('payload: round-trips a server document into PUT shape', () => {
  const p = voiceSettingsPayload(serverSettings());
  assertEqual(p.active_mode, 'code');
  assertEqual(p.auto_mode, true);
  assertEqual(p.history_cap, 500);
  assertEqual(p.modes.length, 2);
  assertEqual(p.replacements, [{ from: 'clod', to: 'Claude' }]);
  assertEqual(p.vocabulary, ['Tailscale', 'whisper.cpp']);
  assertEqual(p.ai, { provider: 'anthropic', model: 'claude-sonnet-4-5' });
});

test('payload: NEVER carries has_key or default_models back to the server', () => {
  const wire = JSON.stringify(voiceSettingsPayload(serverSettings()));
  assertTrue(wire.indexOf('has_key') === -1, `has_key leaked into the payload: ${wire}`);
  assertTrue(wire.indexOf('default_models') === -1, `default_models leaked: ${wire}`);
});

test('payload: NEVER carries an API key, even if one is jammed into state', () => {
  const dirty = serverSettings();
  dirty.ai.key = 'sk-ant-do-not-send-me';
  dirty.ai.apiKey = 'sk-also-not-this';
  dirty.key = 'sk-nor-this';
  const p = voiceSettingsPayload(dirty);
  const wire = JSON.stringify(p);
  assertTrue(wire.indexOf('sk-') === -1, `a key leaked into the payload: ${wire}`);
  assertEqual(Object.keys(p.ai).sort(), ['model', 'provider']);
  assertEqual(Object.keys(p).sort(), [
    'active_mode', 'ai', 'auto_mode', 'history_cap', 'modes', 'replacements', 'vocabulary',
  ]);
});

test('payload: a mode keeps exactly the seven contract fields', () => {
  const dirty = serverSettings();
  dirty.modes[0].secret = 'nope';
  const p = voiceSettingsPayload(dirty);
  assertEqual(Object.keys(p.modes[0]).sort(), [
    'apps', 'id', 'name', 'prompt', 'use_context', 'use_replacements', 'use_vocabulary',
  ]);
});

test('payload: an active_mode pointing at a deleted mode falls back to the first', () => {
  const s = serverSettings();
  s.active_mode = 'deleted-mode';
  assertEqual(voiceSettingsPayload(s).active_mode, 'email');
});

test('payload: no modes at all means no active mode, not a dangling id', () => {
  const p = voiceSettingsPayload({ modes: [], active_mode: 'email' });
  assertEqual(p.active_mode, '');
  assertEqual(p.modes, []);
});

test('payload: blank replacements and vocabulary entries are dropped', () => {
  const p = voiceSettingsPayload({
    replacements: [{ from: '  ', to: 'x' }, { from: ' clod ', to: 'Claude' }, {}],
    vocabulary: ['  ', 'Helius', '', '  Helius  ', 'helius'],
  });
  assertEqual(p.replacements, [{ from: 'clod', to: 'Claude' }]);
  assertEqual(p.vocabulary, ['Helius'], 'duplicates should collapse case-insensitively');
});

test('payload: an unknown provider degrades to none rather than being sent on', () => {
  assertEqual(voiceSettingsPayload({ ai: { provider: 'skynet' } }).ai.provider, 'none');
  assertEqual(voiceSettingsPayload({}).ai.provider, 'none');
});

test('payload: history cap from a text input is coerced and clamped', () => {
  assertEqual(voiceSettingsPayload({ history_cap: '250' }).history_cap, 250);
  assertEqual(voiceSettingsPayload({ history_cap: -5 }).history_cap, 0);
  assertEqual(voiceSettingsPayload({ history_cap: 999999 }).history_cap, 10000);
  assertEqual(voiceSettingsPayload({ history_cap: 'abc' }).history_cap, 200);
  assertEqual(voiceSettingsPayload({}).history_cap, 200);
});

test('payload: survives being handed nothing at all', () => {
  const p = voiceSettingsPayload(undefined);
  assertEqual(p.modes, []);
  assertEqual(p.vocabulary, []);
  assertEqual(p.ai, { provider: 'none', model: '' });
});

test('payload: is idempotent -- saving its own output changes nothing', () => {
  const once = voiceSettingsPayload(serverSettings());
  assertEqual(voiceSettingsPayload(once), once);
});

// ---- the key field ----------------------------------------------------------

test('key field: a stored key advertises replacement, never a value', () => {
  const st = voiceKeyFieldState({ anthropic: true }, 'anthropic');
  assertEqual(st.saved, true);
  assertEqual(st.enabled, true);
  assertEqual(st.label, 'Key saved ✓ (replace?)');
  assertTrue(st.placeholder.indexOf('replace') !== -1, 'placeholder should invite a replacement');
});

test('key field: no stored key asks for one plainly', () => {
  assertEqual(voiceKeyFieldState({ anthropic: true }, 'openai'), {
    enabled: true, saved: false, label: 'API key', placeholder: 'Paste your API key',
  });
});

test('key field: the "none" provider needs no key at all', () => {
  assertEqual(voiceKeyFieldState({ anthropic: true }, 'none'), {
    enabled: false, saved: false, label: 'No API key needed', placeholder: '',
  });
});

test('key field: a missing has_key map is treated as "no key", not a crash', () => {
  assertEqual(voiceKeyFieldState(undefined, 'gemini').saved, false);
  assertEqual(voiceKeyFieldState(null, 'gemini').saved, false);
});

test('key field: every real provider is spelled the way the contract spells it', () => {
  assertEqual(VOICE_PROVIDERS, ['none', 'anthropic', 'openai', 'gemini', 'openrouter']);
  assertEqual(voiceProviderLabel('anthropic'), 'Claude');
  assertEqual(voiceNormalizeProvider('OpenAI'), 'none', 'matching is exact, not case-folded');
});

// ---- model placeholder ------------------------------------------------------

test('model placeholder: shows the selected provider default', () => {
  const d = serverSettings().ai.default_models;
  assertEqual(voiceModelPlaceholder(d, 'anthropic'), 'claude-haiku-4-5');
  assertEqual(voiceModelPlaceholder(d, 'gemini'), 'gemini-2.5-flash');
});

test('model placeholder: is blank when no AI step will run', () => {
  assertEqual(voiceModelPlaceholder(serverSettings().ai.default_models, 'none'), '');
});

test('model placeholder: falls back when the server omits a default', () => {
  assertEqual(voiceModelPlaceholder({}, 'openai'), 'provider default');
  assertEqual(voiceModelPlaceholder(undefined, 'openai'), 'provider default');
  assertEqual(voiceModelPlaceholder({ openai: '' }, 'openai'), 'provider default');
});

// ---- relative time ----------------------------------------------------------

const NOW = 1_700_000_000_000;

test('time: a fresh dictation reads as "just now"', () => {
  assertEqual(voiceRelativeTime(NOW, NOW), 'just now');
  assertEqual(voiceRelativeTime(NOW - 9_000, NOW), 'just now');
});

test('time: a phone clock running slightly fast still reads "just now"', () => {
  assertEqual(voiceRelativeTime(NOW + 4_000, NOW), 'just now');
});

test('time: seconds, then minutes, at the boundary', () => {
  assertEqual(voiceRelativeTime(NOW - 10_000, NOW), '10s ago');
  assertEqual(voiceRelativeTime(NOW - 59_000, NOW), '59s ago');
  assertEqual(voiceRelativeTime(NOW - 60_000, NOW), '1m ago');
  assertEqual(voiceRelativeTime(NOW - 120_000, NOW), '2m ago');
});

test('time: minutes roll into hours at exactly 60', () => {
  assertEqual(voiceRelativeTime(NOW - 59 * 60_000, NOW), '59m ago');
  assertEqual(voiceRelativeTime(NOW - 60 * 60_000, NOW), '1h ago');
  assertEqual(voiceRelativeTime(NOW - 23 * 3600_000, NOW), '23h ago');
});

test('time: hours roll into days at exactly 24', () => {
  assertEqual(voiceRelativeTime(NOW - 24 * 3600_000, NOW), '1d ago');
  assertEqual(voiceRelativeTime(NOW - 3 * 86400_000, NOW), '3d ago');
  assertEqual(voiceRelativeTime(NOW - 6 * 86400_000, NOW), '6d ago');
});

test('time: a week or more stops counting days', () => {
  assertEqual(voiceRelativeTime(NOW - 7 * 86400_000, NOW), '1w ago');
  assertEqual(voiceRelativeTime(NOW - 20 * 86400_000, NOW), '2w ago');
  assertEqual(voiceRelativeTime(NOW - 200 * 86400_000, NOW), '6mo ago');
});

test('time: a seconds-epoch timestamp is not read as 1970', () => {
  assertEqual(voiceAtMs(1_700_000_000), 1_700_000_000_000);
  assertEqual(voiceAtMs(1_700_000_000_000), 1_700_000_000_000);
  assertEqual(voiceRelativeTime(1_699_999_940, NOW), '1m ago');
});

test('time: junk timestamps produce nothing rather than "NaN ago"', () => {
  assertEqual(voiceRelativeTime(undefined, NOW), '');
  assertEqual(voiceRelativeTime('soon', NOW), '');
  assertEqual(voiceRelativeTime(0, NOW), '');
});

// ---- clip length ------------------------------------------------------------

test('clip length: compact seconds and minutes', () => {
  assertEqual(voiceClipLength(4.25), '4.3s');
  assertEqual(voiceClipLength(59.9), '59.9s');
  assertEqual(voiceClipLength(60), '1m');
  assertEqual(voiceClipLength(95), '1m 35s');
});

test('clip length: never renders "1m 60s"', () => {
  assertEqual(voiceClipLength(119.7), '2m');
});

test('clip length: missing or nonsense values show a dash', () => {
  assertEqual(voiceClipLength(0), '—');
  assertEqual(voiceClipLength(undefined), '—');
  assertEqual(voiceClipLength(-2), '—');
  assertEqual(voiceClipLength('12'), '12s', 'numeric strings from JSON are still numbers');
});

// ---- preview ----------------------------------------------------------------

test('preview: short text passes through untouched', () => {
  assertEqual(voicePreview('hello world'), 'hello world');
});

test('preview: newlines and runs of spaces collapse to one line', () => {
  assertEqual(voicePreview('  hello\n\n  world \t there  '), 'hello world there');
});

test('preview: a very long transcript is truncated with an ellipsis', () => {
  const long = 'word '.repeat(200).trim();
  const p = voicePreview(long);
  assertTrue(p.length <= 90, `preview should fit the row, got ${p.length} chars`);
  assertTrue(p.endsWith('…'), `expected an ellipsis, got ${JSON.stringify(p.slice(-5))}`);
  assertTrue(!p.endsWith(' …'), 'no dangling space before the ellipsis');
});

test('preview: truncation respects a custom limit and never splits mid-space', () => {
  assertEqual(voicePreview('alpha beta gamma', 10), 'alpha…');
});

test('preview: empty text says so instead of rendering a blank row', () => {
  assertEqual(voicePreview(''), '(no text)');
  assertEqual(voicePreview(null), '(no text)');
  assertEqual(voicePreview('   \n  '), '(no text)');
});

// ---- history rows -----------------------------------------------------------

test('history row: formats time, mode, duration and preview together', () => {
  assertEqual(voiceHistoryRow({
    id: 42, at: NOW - 120_000, mode: 'Email', seconds: 95, raw: 'raw one', text: 'Processed one',
  }, NOW), {
    id: '42', when: '2m ago', mode: 'Email', duration: '1m 35s', preview: 'Processed one',
  });
});

test('history row: falls back to the raw transcript when nothing was processed', () => {
  const r = voiceHistoryRow({ id: 'a', at: NOW, seconds: 3, raw: 'just the raw', text: '' }, NOW);
  assertEqual(r.preview, 'just the raw');
  assertEqual(r.mode, 'raw', 'a mode-less entry is labelled raw, not blank');
});

test('history row: survives a half-empty entry', () => {
  assertEqual(voiceHistoryRow({}, NOW), {
    id: '', when: '', mode: 'raw', duration: '—', preview: '(no text)',
  });
});

// ---- the dictation-done view ------------------------------------------------

test('done: a clean run shows the text and no notice', () => {
  assertEqual(voiceDoneView({ text: 'Hello there.', raw: 'hello there', mode: 'Email', warning: null }), {
    show: true,
    text: 'Hello there.',
    raw: 'hello there',
    mode: 'Email',
    notice: null,
    copyable: true,
    state: 'typed into your PC',
  });
});

test('done: a warning does NOT hide the text -- it stays shown and copyable', () => {
  const v = voiceDoneView({
    text: 'hello there',
    raw: 'hello there',
    mode: 'Email',
    warning: 'AI step failed; delivered raw transcript',
  });
  assertEqual(v.show, true, 'the transcript must still render');
  assertEqual(v.text, 'hello there');
  assertEqual(v.copyable, true, 'the user must still be able to copy it');
  assertEqual(v.notice, 'AI step failed; delivered raw transcript');
  assertEqual(v.state, 'delivered raw transcript');
});

test('done: identical raw and processed text does not render the same block twice', () => {
  assertEqual(voiceDoneView({ text: 'same', raw: 'same' }).raw, null);
  assertEqual(voiceDoneView({ text: 'processed', raw: 'raw' }).raw, 'raw');
});

test('done: empty text with no warning reports nothing heard', () => {
  const v = voiceDoneView({ text: '', warning: null, mode: 'Email' });
  assertEqual(v.show, false);
  assertEqual(v.copyable, false);
  assertEqual(v.notice, null);
  assertEqual(v.state, 'nothing heard');
});

test('done: empty text WITH a warning surfaces the warning as the status', () => {
  const v = voiceDoneView({ text: '', warning: 'AI step failed' });
  assertEqual(v.show, false);
  assertEqual(v.notice, 'AI step failed');
  assertEqual(v.state, 'AI step failed');
});

test('done: a blank-string warning is not a warning', () => {
  assertEqual(voiceDoneView({ text: 'hi', warning: '   ' }).notice, null);
  assertEqual(voiceDoneView({ text: 'hi', warning: '' }).state, 'typed into your PC');
});

test('done: missing fields are treated as empty, not undefined', () => {
  assertEqual(voiceDoneView({}), {
    show: false, text: '', raw: null, mode: null, notice: null, copyable: false,
    state: 'nothing heard',
  });
  assertEqual(voiceDoneView(null).show, false);
});

test('done: a mode name is trimmed and a blank one is dropped', () => {
  assertEqual(voiceDoneView({ text: 'x', mode: '  Email  ' }).mode, 'Email');
  assertEqual(voiceDoneView({ text: 'x', mode: '   ' }).mode, null);
});

// ---- search filtering -------------------------------------------------------

const HISTORY = [
  { id: '1', mode: 'Email', raw: 'send bob the invoice', text: 'Send Bob the invoice.' },
  { id: '2', mode: 'Code', raw: 'refactor the parser', text: 'Refactor the parser.' },
  { id: '3', mode: 'Email', raw: 'lunch tomorrow', text: 'Lunch tomorrow?' },
];

test('search: an empty query returns everything, in order', () => {
  assertEqual(voiceFilterHistory(HISTORY, '').map((e) => e.id), ['1', '2', '3']);
  assertEqual(voiceFilterHistory(HISTORY, '   ').map((e) => e.id), ['1', '2', '3']);
  assertEqual(voiceFilterHistory(HISTORY, null).map((e) => e.id), ['1', '2', '3']);
});

test('search: an empty query returns a copy, not the live array', () => {
  const out = voiceFilterHistory(HISTORY, '');
  out.pop();
  assertEqual(HISTORY.length, 3, 'filtering must not mutate the caller list');
});

test('search: matches processed text, raw text and mode, case-insensitively', () => {
  assertEqual(voiceFilterHistory(HISTORY, 'BOB').map((e) => e.id), ['1']);
  assertEqual(voiceFilterHistory(HISTORY, 'parser').map((e) => e.id), ['2']);
  assertEqual(voiceFilterHistory(HISTORY, 'email').map((e) => e.id), ['1', '3']);
});

test('search: no match yields an empty list, not everything', () => {
  assertEqual(voiceFilterHistory(HISTORY, 'zzzz'), []);
});

test('search: a missing list is not a crash', () => {
  assertEqual(voiceFilterHistory(undefined, 'bob'), []);
  assertEqual(voiceFilterHistory(null, ''), []);
});

// ---- mode chips -------------------------------------------------------------

test('chips: exactly one chip is active', () => {
  const chips = voiceModeChips(serverSettings().modes, 'code');
  assertEqual(chips.map((c) => c.active), [false, true]);
  assertEqual(chips.map((c) => c.name), ['Email', 'Code']);
});

test('chips: an unknown active id highlights the first rather than nothing', () => {
  assertEqual(voiceModeChips(serverSettings().modes, 'gone').map((c) => c.active), [true, false]);
  assertEqual(voiceModeChips(serverSettings().modes, null).map((c) => c.active), [true, false]);
});

test('chips: an unnamed mode falls back to its id', () => {
  assertEqual(voiceModeChips([{ id: 'raw' }], 'raw'), [{ id: 'raw', name: 'raw', active: true }]);
});

test('chips: no modes yields an empty row for the caller to empty-state', () => {
  assertEqual(voiceModeChips([], 'x'), []);
  assertEqual(voiceModeChips(undefined, 'x'), []);
});

// ---- stats ------------------------------------------------------------------

test('stats: renders the three headline numbers', () => {
  assertEqual(
    voiceStatCards({ total: 42, total_words: 900, words_this_week: 310, minutes_saved: 18.6 }),
    [
      { label: 'dictations', value: '42' },
      { label: 'words this week', value: '310' },
      { label: 'minutes saved', value: '19' },
    ],
  );
});

test('stats: a fresh install shows zeros, not blanks or NaN', () => {
  assertEqual(voiceStatCards({}).map((c) => c.value), ['0', '0', '0']);
  assertEqual(voiceStatCards(null).map((c) => c.value), ['0', '0', '0']);
});

// ---- mode ids ---------------------------------------------------------------

test('mode id: slugifies a typed name', () => {
  assertEqual(voiceModeId('Work Email!', 1700000000000), 'work-email-3v28');
});

test('mode id: two modes with the same name get different ids', () => {
  assertTrue(
    voiceModeId('Email', 1700000000000) !== voiceModeId('Email', 1700000001000),
    'ids must not collide across time',
  );
});

test('mode id: a name with no usable characters still yields an id', () => {
  assertTrue(voiceModeId('!!!', 1700000000000).startsWith('mode-'));
  assertTrue(voiceModeId('', 1700000000000).startsWith('mode-'));
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
