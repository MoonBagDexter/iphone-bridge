// Plain-Node test for the pure helpers backing the Files tab in web/app.js.
// Run: node tests/files-ui.test.mjs
//
// Loads app.js as text and evaluates ONLY the code between the
// `files-ui-pure` markers, so this test exercises the exact helpers shipped
// to the browser without needing a DOM or bundler. Same technique as
// tests/ptt-press.test.mjs -- read that file for the pattern this mirrors.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import vm from 'node:vm';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const appJsPath = path.join(__dirname, '..', 'web', 'app.js');
const src = readFileSync(appJsPath, 'utf8');

const START_MARKER = '// --- files-ui-pure (pure; tested by tests/files-ui.test.mjs) ---';
const END_MARKER = '// --- end files-ui-pure ---';

const startIdx = src.indexOf(START_MARKER);
const endIdx = src.indexOf(END_MARKER);
if (startIdx === -1 || endIdx === -1 || endIdx <= startIdx) {
  throw new Error('Could not locate files-ui-pure markers in web/app.js');
}
const helperSrc = src.slice(startIdx, endIdx);

const sandbox = {};
vm.createContext(sandbox);
vm.runInContext(
  `${helperSrc}\n` +
  `this.filesBreadcrumbs = filesBreadcrumbs;\n` +
  `this.updateRecents = updateRecents;\n` +
  `this.annotateEntries = annotateEntries;\n` +
  `this.formatRelativeTime = formatRelativeTime;\n` +
  `this.formatTrashItem = formatTrashItem;\n` +
  `this.buildShortcutChips = buildShortcutChips;\n` +
  `this.nextLastLocation = nextLastLocation;\n` +
  `this.resolveRestoreLocation = resolveRestoreLocation;\n` +
  `this.filesSortComparator = filesSortComparator;\n` +
  `this.formatDiskSpace = formatDiskSpace;\n` +
  `this.gitStatusMeta = gitStatusMeta;\n` +
  `this.peekLineModel = peekLineModel;\n` +
  `this.uploadSummaryText = uploadSummaryText;\n` +
  `this.spawnToastText = spawnToastText;\n` +
  `this.searchResultDisplay = searchResultDisplay;\n` +
  `this.twoTapReduce = twoTapReduce;\n` +
  `this.migrateHistory = migrateHistory;\n` +
  `this.updateHistory = updateHistory;\n` +
  `this.historyEntryModel = historyEntryModel;\n` +
  `this.subViewVisibility = subViewVisibility;\n`,
  sandbox
);
const { filesBreadcrumbs, updateRecents, annotateEntries, formatRelativeTime, formatTrashItem, buildShortcutChips, nextLastLocation, resolveRestoreLocation, filesSortComparator, formatDiskSpace, gitStatusMeta, peekLineModel, uploadSummaryText, spawnToastText, searchResultDisplay, twoTapReduce, migrateHistory, updateHistory, historyEntryModel, subViewVisibility } = sandbox;

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

// ---- filesBreadcrumbs -------------------------------------------------------

test('breadcrumbs: root path alone yields a single crumb (the root name)', () => {
  const roots = [{ name: 'Desktop', path: 'C:\\Users\\thedi\\Desktop' }];
  const crumbs = filesBreadcrumbs('C:\\Users\\thedi\\Desktop', roots);
  assertEqual(crumbs, [{ label: 'Desktop', path: 'C:\\Users\\thedi\\Desktop' }]);
});

test('breadcrumbs: nested path segments each get a tappable cumulative path', () => {
  const roots = [{ name: 'Desktop', path: 'C:\\Users\\thedi\\Desktop' }];
  const crumbs = filesBreadcrumbs('C:\\Users\\thedi\\Desktop\\Work\\Tools', roots);
  assertEqual(crumbs, [
    { label: 'Desktop', path: 'C:\\Users\\thedi\\Desktop' },
    { label: 'Work', path: 'C:\\Users\\thedi\\Desktop\\Work' },
    { label: 'Tools', path: 'C:\\Users\\thedi\\Desktop\\Work\\Tools' },
  ]);
});

test('breadcrumbs: picks the longest-matching root when roots overlap', () => {
  const roots = [
    { name: 'Users', path: 'C:\\Users' },
    { name: 'thedi', path: 'C:\\Users\\thedi' },
  ];
  const crumbs = filesBreadcrumbs('C:\\Users\\thedi\\Desktop', roots);
  assertEqual(crumbs, [
    { label: 'thedi', path: 'C:\\Users\\thedi' },
    { label: 'Desktop', path: 'C:\\Users\\thedi\\Desktop' },
  ]);
});

test('breadcrumbs: root match is case-insensitive (Windows paths)', () => {
  const roots = [{ name: 'Desktop', path: 'c:\\users\\thedi\\desktop' }];
  const crumbs = filesBreadcrumbs('C:\\Users\\thedi\\Desktop\\Mic', roots);
  assertEqual(crumbs, [
    { label: 'Desktop', path: 'c:\\users\\thedi\\desktop' },
    { label: 'Mic', path: 'c:\\users\\thedi\\desktop\\Mic' },
  ]);
});

test('breadcrumbs: no matching root falls back to the raw path as one crumb', () => {
  const crumbs = filesBreadcrumbs('D:\\Elsewhere\\Stuff', []);
  assertEqual(crumbs, [{ label: 'D:\\Elsewhere\\Stuff', path: 'D:\\Elsewhere\\Stuff' }]);
});

// ---- updateRecents -----------------------------------------------------------

test('recents: pushes a new path to the front', () => {
  const result = updateRecents([], 'C:\\a');
  assertEqual(result, ['C:\\a']);
});

test('recents: most-recent-first ordering', () => {
  let result = updateRecents([], 'C:\\a');
  result = updateRecents(result, 'C:\\b');
  assertEqual(result, ['C:\\b', 'C:\\a']);
});

test('recents: re-navigating to an existing entry dedupes and moves it to front', () => {
  let result = ['C:\\c', 'C:\\b', 'C:\\a'];
  result = updateRecents(result, 'C:\\b');
  assertEqual(result, ['C:\\b', 'C:\\c', 'C:\\a']);
});

test('recents: dedupe is case-insensitive on Windows paths', () => {
  let result = ['C:\\Users\\thedi\\Desktop'];
  result = updateRecents(result, 'c:\\users\\thedi\\desktop');
  assertEqual(result, ['c:\\users\\thedi\\desktop']);
});

test('recents: caps at 8 entries, dropping the oldest', () => {
  let result = [];
  for (let i = 1; i <= 8; i++) result = updateRecents(result, `C:\\p${i}`);
  assertEqual(result.length, 8);
  result = updateRecents(result, 'C:\\p9');
  assertEqual(result.length, 8);
  assertEqual(result[0], 'C:\\p9');
  assertEqual(result.includes('C:\\p1'), false, 'oldest entry should be evicted once past the cap');
});

test('recents: does not mutate the input array', () => {
  const input = ['C:\\a'];
  const result = updateRecents(input, 'C:\\b');
  assertEqual(input, ['C:\\a'], 'original array must be untouched');
  assertEqual(result, ['C:\\b', 'C:\\a']);
});

// ---- annotateEntries ---------------------------------------------------------

test('annotate: plain file entry gets no dirty flag and favorite=false', () => {
  const entries = [{ name: 'readme.txt', path: 'C:\\p\\readme.txt', isDir: false, isRepo: false, mtimeMs: 1 }];
  const out = annotateEntries(entries, {}, []);
  assertEqual(out, [{ name: 'readme.txt', path: 'C:\\p\\readme.txt', isDir: false, isRepo: false, mtimeMs: 1, dirty: null, favorite: false }]);
});

test('annotate: repo entry with a cached gitstatus gets dirty populated', () => {
  const entries = [{ name: 'proj', path: 'C:\\p\\proj', isDir: true, isRepo: true, mtimeMs: 1 }];
  const cache = { 'C:\\p\\proj': true };
  const out = annotateEntries(entries, cache, []);
  assertEqual(out[0].dirty, true);
});

test('annotate: repo entry with no cache entry yet gets dirty=null (not yet fetched)', () => {
  const entries = [{ name: 'proj', path: 'C:\\p\\proj', isDir: true, isRepo: true, mtimeMs: 1 }];
  const out = annotateEntries(entries, {}, []);
  assertEqual(out[0].dirty, null);
});

test('annotate: non-repo entry ignores the gitstatus cache even if a stale key exists', () => {
  const entries = [{ name: 'notrepo', path: 'C:\\p\\notrepo', isDir: true, isRepo: false, mtimeMs: 1 }];
  const cache = { 'C:\\p\\notrepo': true };
  const out = annotateEntries(entries, cache, []);
  assertEqual(out[0].dirty, null);
});

test('annotate: favorite paths are flagged true, case-insensitively', () => {
  const entries = [{ name: 'Desktop', path: 'C:\\Users\\thedi\\Desktop', isDir: true, isRepo: false, mtimeMs: 1 }];
  const favorites = ['c:\\users\\thedi\\desktop'];
  const out = annotateEntries(entries, {}, favorites);
  assertEqual(out[0].favorite, true);
});

test('annotate: does not mutate the input entries array/objects', () => {
  const entries = [{ name: 'proj', path: 'C:\\p\\proj', isDir: true, isRepo: true, mtimeMs: 1 }];
  const cache = { 'C:\\p\\proj': true };
  annotateEntries(entries, cache, ['C:\\p\\proj']);
  assertEqual(entries, [{ name: 'proj', path: 'C:\\p\\proj', isDir: true, isRepo: true, mtimeMs: 1 }], 'original entry objects must be untouched');
});

// ---- formatRelativeTime -------------------------------------------------------

test('relative time: null deletedAtMs yields an em dash placeholder', () => {
  const now = Date.parse('2026-07-05T12:00:00Z');
  assertEqual(formatRelativeTime(null, now), '—');
});

test('relative time: under a minute shows as "just now"', () => {
  const now = Date.parse('2026-07-05T12:00:00Z');
  const deletedAtMs = now - 10 * 1000;
  assertEqual(formatRelativeTime(deletedAtMs, now), 'just now');
});

test('relative time: minutes ago', () => {
  const now = Date.parse('2026-07-05T12:00:00Z');
  const deletedAtMs = now - 5 * 60 * 1000;
  assertEqual(formatRelativeTime(deletedAtMs, now), '5m ago');
});

test('relative time: hours ago', () => {
  const now = Date.parse('2026-07-05T12:00:00Z');
  const deletedAtMs = now - 2 * 60 * 60 * 1000;
  assertEqual(formatRelativeTime(deletedAtMs, now), '2h ago');
});

test('relative time: days ago (within the last week)', () => {
  const now = Date.parse('2026-07-05T12:00:00Z');
  const deletedAtMs = now - 3 * 24 * 60 * 60 * 1000;
  assertEqual(formatRelativeTime(deletedAtMs, now), '3d ago');
});

test('relative time: beyond 7 days falls back to an absolute date', () => {
  const now = Date.parse('2026-07-05T12:00:00Z');
  const deletedAtMs = Date.parse('2026-06-01T09:00:00Z');
  const result = formatRelativeTime(deletedAtMs, now);
  // Absolute date -- not a relative "Nd ago"/"Nh ago" string.
  assertEqual(/ago$/.test(result), false, 'should not use relative phrasing past 7 days');
  assertEqual(result.length > 0, true);
});

// ---- formatTrashItem ---------------------------------------------------------

test('trash item: null size formats to empty string', () => {
  const item = { id: '1', name: 'foo.txt', originalPath: 'C:\\a\\foo.txt', deletedAtMs: null, sizeBytes: null, isDir: false };
  const out = formatTrashItem(item, Date.now());
  assertEqual(out.sizeLabel, '');
});

test('trash item: sizeBytes under 1024 shows raw bytes', () => {
  const item = { id: '1', name: 'foo.txt', originalPath: 'C:\\a\\foo.txt', deletedAtMs: null, sizeBytes: 500, isDir: false };
  const out = formatTrashItem(item, Date.now());
  assertEqual(out.sizeLabel, '500 B');
});

test('trash item: sizeBytes in KB range formats with KB suffix', () => {
  const item = { id: '1', name: 'foo.txt', originalPath: 'C:\\a\\foo.txt', deletedAtMs: null, sizeBytes: 2048, isDir: false };
  const out = formatTrashItem(item, Date.now());
  assertEqual(out.sizeLabel, '2 KB');
});

test('trash item: sizeBytes in MB range formats with MB suffix', () => {
  const item = { id: '1', name: 'foo.txt', originalPath: 'C:\\a\\foo.txt', deletedAtMs: null, sizeBytes: 5 * 1024 * 1024, isDir: false };
  const out = formatTrashItem(item, Date.now());
  assertEqual(out.sizeLabel, '5 MB');
});

test('trash item: isDir flag passes through unchanged', () => {
  const item = { id: '1', name: 'folder', originalPath: 'C:\\a\\folder', deletedAtMs: null, sizeBytes: null, isDir: true };
  const out = formatTrashItem(item, Date.now());
  assertEqual(out.isDir, true);
});

test('trash item: name and originalPath pass through unchanged', () => {
  const item = { id: '1', name: 'foo.txt', originalPath: 'C:\\a\\foo.txt', deletedAtMs: null, sizeBytes: null, isDir: false };
  const out = formatTrashItem(item, Date.now());
  assertEqual(out.name, 'foo.txt');
  assertEqual(out.originalPath, 'C:\\a\\foo.txt');
});

test('trash item: deletedAtMs is formatted through formatRelativeTime', () => {
  const now = Date.parse('2026-07-05T12:00:00Z');
  const deletedAtMs = now - 60 * 60 * 1000;
  const item = { id: '1', name: 'foo.txt', originalPath: 'C:\\a\\foo.txt', deletedAtMs, sizeBytes: null, isDir: false };
  const out = formatTrashItem(item, now);
  assertEqual(out.deletedLabel, '1h ago');
});

// ---- buildShortcutChips --------------------------------------------------------

test('shortcut chips: empty shortcuts list still yields the Trash chip (Trash always present)', () => {
  const out = buildShortcutChips([]);
  assertEqual(out, [{ name: 'Trash', path: null, isTrash: true }]);
});

test('shortcut chips: null shortcuts still yields the Trash chip', () => {
  const out = buildShortcutChips(null);
  assertEqual(out, [{ name: 'Trash', path: null, isTrash: true }]);
});

test('shortcut chips: maps name/path through and appends a trailing Trash pseudo-chip', () => {
  const shortcuts = [{ name: 'Desktop', path: 'C:\\Users\\thedi\\Desktop' }];
  const out = buildShortcutChips(shortcuts);
  assertEqual(out, [
    { name: 'Desktop', path: 'C:\\Users\\thedi\\Desktop', isTrash: false },
    { name: 'Trash', path: null, isTrash: true },
  ]);
});

test('shortcut chips: multiple shortcuts preserve order, Trash chip still last', () => {
  const shortcuts = [
    { name: 'Desktop', path: 'C:\\Users\\thedi\\Desktop' },
    { name: 'Downloads', path: 'C:\\Users\\thedi\\Downloads' },
  ];
  const out = buildShortcutChips(shortcuts);
  assertEqual(out.length, 3);
  assertEqual(out[0].name, 'Desktop');
  assertEqual(out[1].name, 'Downloads');
  assertEqual(out[2].isTrash, true);
});

// ---- nextLastLocation ---------------------------------------------------------

test('last location: roots view stores path=null, view=browser', () => {
  assertEqual(nextLastLocation('browser', null), { view: 'browser', path: null });
});

test('last location: a browsed folder stores its path with view=browser', () => {
  assertEqual(nextLastLocation('browser', 'C:\\Users\\thedi\\Desktop'), { view: 'browser', path: 'C:\\Users\\thedi\\Desktop' });
});

test('last location: trash view stores view=trash regardless of path', () => {
  assertEqual(nextLastLocation('trash', null), { view: 'trash', path: null });
});

// ---- resolveRestoreLocation ---------------------------------------------------

test('restore: no stored value falls back to roots', () => {
  assertEqual(resolveRestoreLocation(null), { view: 'browser', path: null });
});

test('restore: malformed stored value (not an object) falls back to roots', () => {
  assertEqual(resolveRestoreLocation('garbage'), { view: 'browser', path: null });
});

test('restore: missing/invalid view falls back to roots but keeps a valid path', () => {
  assertEqual(resolveRestoreLocation({ path: 'C:\\a' }), { view: 'browser', path: 'C:\\a' });
});

test('restore: stored browser location with a path is preserved', () => {
  assertEqual(resolveRestoreLocation({ view: 'browser', path: 'C:\\Users\\thedi\\Desktop' }),
    { view: 'browser', path: 'C:\\Users\\thedi\\Desktop' });
});

test('restore: stored trash view is preserved', () => {
  assertEqual(resolveRestoreLocation({ view: 'trash', path: null }), { view: 'trash', path: null });
});

test('restore: non-string path is treated as absent (falls back to roots path)', () => {
  assertEqual(resolveRestoreLocation({ view: 'browser', path: 123 }), { view: 'browser', path: null });
});

// ---- filesSortComparator -----------------------------------------------------
// A comparator factory: filesSortComparator(mode) returns (a, b) => number.
// mode 'name' -> dirs before files, then case-insensitive name asc.
// mode 'modified' -> dirs before files, then mtimeMs desc (newest first).

function sortNames(entries, mode) {
  return entries.slice().sort(filesSortComparator(mode)).map((e) => e.name);
}

test('sort/name: directories sort before files', () => {
  const entries = [
    { name: 'a.txt', isDir: false, mtimeMs: 5 },
    { name: 'zdir', isDir: true, mtimeMs: 1 },
  ];
  assertEqual(sortNames(entries, 'name'), ['zdir', 'a.txt']);
});

test('sort/name: within a kind, case-insensitive ascending', () => {
  const entries = [
    { name: 'Banana', isDir: false, mtimeMs: 1 },
    { name: 'apple', isDir: false, mtimeMs: 2 },
    { name: 'Cherry', isDir: false, mtimeMs: 3 },
  ];
  assertEqual(sortNames(entries, 'name'), ['apple', 'Banana', 'Cherry']);
});

test('sort/modified: dirs still before files, then newest mtime first', () => {
  const entries = [
    { name: 'oldfile', isDir: false, mtimeMs: 10 },
    { name: 'newfile', isDir: false, mtimeMs: 90 },
    { name: 'olddir', isDir: true, mtimeMs: 5 },
    { name: 'newdir', isDir: true, mtimeMs: 50 },
  ];
  assertEqual(sortNames(entries, 'modified'), ['newdir', 'olddir', 'newfile', 'oldfile']);
});

test('sort/modified: missing mtimeMs is treated as 0 (sorts last within kind)', () => {
  const entries = [
    { name: 'has', isDir: false, mtimeMs: 5 },
    { name: 'missing', isDir: false },
  ];
  assertEqual(sortNames(entries, 'modified'), ['has', 'missing']);
});

test('sort: unknown mode falls back to name ordering', () => {
  const entries = [
    { name: 'b', isDir: false, mtimeMs: 9 },
    { name: 'a', isDir: false, mtimeMs: 1 },
  ];
  assertEqual(sortNames(entries, 'whatever'), ['a', 'b']);
});

// ---- formatDiskSpace ---------------------------------------------------------

test('disk: null free/total yields empty string', () => {
  assertEqual(formatDiskSpace(null, null), '');
  assertEqual(formatDiskSpace(null, 100), '');
  assertEqual(formatDiskSpace(100, null), '');
});

test('disk: GB range shows one decimal', () => {
  const gb = 1024 * 1024 * 1024;
  // one decimal on both sides, consistently
  assertEqual(formatDiskSpace(123.4 * gb, 500 * gb), '123.4 GB free of 500.0 GB');
});

test('disk: total beyond 1024 GB is shown in TB', () => {
  const gb = 1024 * 1024 * 1024;
  const tb = 1024 * gb;
  // 931 GB free of 2 TB
  const out = formatDiskSpace(931 * gb, 2 * tb);
  assertEqual(out, '931.0 GB free of 2.0 TB');
});

test('disk: free beyond 1024 GB is also shown in TB', () => {
  const gb = 1024 * 1024 * 1024;
  const tb = 1024 * gb;
  const out = formatDiskSpace(1.5 * tb, 3 * tb);
  assertEqual(out, '1.5 TB free of 3.0 TB');
});

// ---- gitStatusMeta -----------------------------------------------------------
// Maps a git short-status code to { label, color } for the git-glance sheet.
// M amber, A green, D red, ?? gray. R and anything else -> gray fallback.

test('git status: M is amber (modified)', () => {
  assertEqual(gitStatusMeta('M').color, 'amber');
});
test('git status: A is green (added)', () => {
  assertEqual(gitStatusMeta('A').color, 'green');
});
test('git status: D is red (deleted)', () => {
  assertEqual(gitStatusMeta('D').color, 'red');
});
test('git status: ?? is gray (untracked)', () => {
  assertEqual(gitStatusMeta('??').color, 'gray');
});
test('git status: R (renamed) and unknown codes fall back to gray', () => {
  assertEqual(gitStatusMeta('R').color, 'gray');
  assertEqual(gitStatusMeta('X').color, 'gray');
});
test('git status: label echoes the raw status code', () => {
  assertEqual(gitStatusMeta('M').label, 'M');
  assertEqual(gitStatusMeta('??').label, '??');
});

// ---- peekLineModel -----------------------------------------------------------
// Maps a session-peek line {role, text} to a display model
// { prefix, text, dim }. user -> prefix 'You:', not dim. assistant -> no
// prefix. A [tool: X] assistant line -> dim.

test('peek: user line gets a "You:" prefix and is not dim', () => {
  const m = peekLineModel({ role: 'user', text: 'hello' });
  assertEqual(m.prefix, 'You:');
  assertEqual(m.text, 'hello');
  assertEqual(m.dim, false);
});

test('peek: assistant line has no prefix and is not dim', () => {
  const m = peekLineModel({ role: 'assistant', text: 'working on it' });
  assertEqual(m.prefix, '');
  assertEqual(m.dim, false);
});

test('peek: assistant [tool: X] line is dim', () => {
  const m = peekLineModel({ role: 'assistant', text: '[tool: Read]' });
  assertEqual(m.dim, true);
  assertEqual(m.prefix, '');
});

test('peek: a tool line from any role is dim', () => {
  const m = peekLineModel({ role: 'user', text: '[tool: Bash]' });
  assertEqual(m.dim, true);
});

// ---- uploadSummaryText -------------------------------------------------------
// Turns an /api/upload response {saved:[], rejected:[{name,reason}]} into a
// single toast string.

test('upload summary: all saved, singular', () => {
  assertEqual(uploadSummaryText({ saved: ['a.jpg'], rejected: [] }), '1 uploaded');
});

test('upload summary: all saved, plural', () => {
  assertEqual(uploadSummaryText({ saved: ['a.jpg', 'b.png', 'c.heic'], rejected: [] }), '3 uploaded');
});

test('upload summary: mixed saved + rejected shows both, rejected names its reason', () => {
  const out = uploadSummaryText({ saved: ['a.jpg'], rejected: [{ name: 'big.mov', reason: 'too big' }] });
  assertEqual(out, '1 uploaded · 1 rejected: too big');
});

test('upload summary: only rejected', () => {
  const out = uploadSummaryText({ saved: [], rejected: [{ name: 'big.mov', reason: 'too big' }] });
  assertEqual(out, '1 rejected: too big');
});

test('upload summary: multiple rejected collapses the reason count', () => {
  const out = uploadSummaryText({ saved: [], rejected: [
    { name: 'a', reason: 'too big' }, { name: 'b', reason: 'bad type' },
  ] });
  assertEqual(out, '2 rejected');
});

test('upload summary: nothing at all', () => {
  assertEqual(uploadSummaryText({ saved: [], rejected: [] }), 'nothing uploaded');
});

// ---- spawnToastText ----------------------------------------------------------
// Picks the toast string from an /api/spawn response {mode, firstTime}.

test('spawn toast: firstTime (visible) points the user to the PC trust prompt', () => {
  const out = spawnToastText({ mode: 'visible', firstTime: true });
  assertEqual(/trust prompt/.test(out), true);
});

test('spawn toast: visible (not first time) still mentions the terminal on the PC', () => {
  const out = spawnToastText({ mode: 'visible', firstTime: false });
  assertEqual(/terminal/.test(out), true);
});

test('spawn toast: hidden points the user to the Code tab', () => {
  const out = spawnToastText({ mode: 'hidden', firstTime: false });
  assertEqual(/Code tab/.test(out), true);
});

// ---- searchResultDisplay -----------------------------------------------------
// Maps an /api/search result {name, path, isDir, parent} to a display model
// { name, parent, isDir, icon }.

test('search result: directory gets a folder icon', () => {
  const out = searchResultDisplay({ name: 'proj', path: 'C:\\a\\proj', isDir: true, parent: 'C:\\a' });
  assertEqual(out.icon, '📁');
  assertEqual(out.isDir, true);
  assertEqual(out.name, 'proj');
  assertEqual(out.parent, 'C:\\a');
});

test('search result: file gets a file icon', () => {
  const out = searchResultDisplay({ name: 'note.txt', path: 'C:\\a\\note.txt', isDir: false, parent: 'C:\\a' });
  assertEqual(out.icon, '📄');
  assertEqual(out.isDir, false);
});

// ---- twoTapReduce ------------------------------------------------------------
// Inline two-tap confirm for destructive Kill buttons (arm -> confirm within
// window, or revert on timeout past the window).

const WIN = 3000;

test('two-tap: first tap arms the button', () => {
  const { state, action } = twoTapReduce({ armed: false, armedAt: 0 }, 'tap', 1000, WIN);
  assertEqual(action, 'arm');
  assertEqual(state.armed, true);
  assertEqual(state.armedAt, 1000);
});

test('two-tap: second tap within the window confirms and disarms', () => {
  let r = twoTapReduce({ armed: false, armedAt: 0 }, 'tap', 1000, WIN);
  r = twoTapReduce(r.state, 'tap', 1000 + 2000, WIN); // 2s later, within 3s
  assertEqual(r.action, 'confirm');
  assertEqual(r.state.armed, false);
});

test('two-tap: a tap after the window re-arms instead of confirming', () => {
  let r = twoTapReduce({ armed: false, armedAt: 0 }, 'tap', 1000, WIN);
  r = twoTapReduce(r.state, 'tap', 1000 + 4000, WIN); // 4s later, past 3s
  assertEqual(r.action, 'arm');
  assertEqual(r.state.armed, true);
  assertEqual(r.state.armedAt, 5000);
});

test('two-tap: timeout past the window reverts to disarmed', () => {
  let r = twoTapReduce({ armed: false, armedAt: 0 }, 'tap', 1000, WIN);
  r = twoTapReduce(r.state, 'timeout', 1000 + 3000, WIN);
  assertEqual(r.action, 'revert');
  assertEqual(r.state.armed, false);
});

test('two-tap: a stale timeout before the window elapses is ignored', () => {
  let r = twoTapReduce({ armed: false, armedAt: 0 }, 'tap', 1000, WIN);
  r = twoTapReduce(r.state, 'timeout', 1000 + 1000, WIN); // only 1s later
  assertEqual(r.action, null);
  assertEqual(r.state.armed, true);
});

test('two-tap: timeout while disarmed is a no-op', () => {
  const r = twoTapReduce({ armed: false, armedAt: 0 }, 'timeout', 9999, WIN);
  assertEqual(r.action, null);
  assertEqual(r.state.armed, false);
});

// ---- migrateHistory ----------------------------------------------------------
// Folder History replaces the old recents chip row. It reads either the new
// `files.history` shape (array of { path, at }) or migrates the legacy
// `files.recents` shape (array of plain path strings, no timestamps) into it.
// Result is always an array of { path, at } (at = null for migrated legacy
// entries with no known visit time), deduped case-insensitively, capped.

test('history migrate: legacy recents (array of strings) becomes {path, at:null} entries in order', () => {
  const legacy = ['C:\\a', 'C:\\b', 'C:\\c'];
  const out = migrateHistory(null, legacy);
  assertEqual(out, [
    { path: 'C:\\a', at: null },
    { path: 'C:\\b', at: null },
    { path: 'C:\\c', at: null },
  ]);
});

test('history migrate: new-shape history is preserved as-is (already {path, at})', () => {
  const stored = [{ path: 'C:\\a', at: 111 }, { path: 'C:\\b', at: 222 }];
  const out = migrateHistory(stored, null);
  assertEqual(out, [{ path: 'C:\\a', at: 111 }, { path: 'C:\\b', at: 222 }]);
});

test('history migrate: new-shape wins over legacy when both present', () => {
  const stored = [{ path: 'C:\\new', at: 9 }];
  const legacy = ['C:\\old'];
  const out = migrateHistory(stored, legacy);
  assertEqual(out, [{ path: 'C:\\new', at: 9 }]);
});

test('history migrate: absent both yields empty array', () => {
  assertEqual(migrateHistory(null, null), []);
  assertEqual(migrateHistory(undefined, undefined), []);
});

test('history migrate: caps a long legacy list at 20', () => {
  const legacy = [];
  for (let i = 1; i <= 30; i++) legacy.push(`C:\\p${i}`);
  const out = migrateHistory(null, legacy);
  assertEqual(out.length, 20);
  assertEqual(out[0].path, 'C:\\p1');
});

test('history migrate: garbage entries are dropped, valid ones kept', () => {
  const stored = [{ path: 'C:\\ok', at: 5 }, { at: 1 }, 'nope', { path: 42 }, null];
  const out = migrateHistory(stored, null);
  assertEqual(out, [{ path: 'C:\\ok', at: 5 }]);
});

// ---- updateHistory -----------------------------------------------------------

test('history update: adds a new visit to the front with its timestamp', () => {
  const out = updateHistory([], 'C:\\a', 1000);
  assertEqual(out, [{ path: 'C:\\a', at: 1000 }]);
});

test('history update: re-visiting dedupes case-insensitively and refreshes the timestamp', () => {
  const hist = [{ path: 'C:\\Users\\thedi\\Desktop', at: 100 }, { path: 'C:\\b', at: 50 }];
  const out = updateHistory(hist, 'c:\\users\\thedi\\desktop', 999);
  assertEqual(out, [
    { path: 'c:\\users\\thedi\\desktop', at: 999 },
    { path: 'C:\\b', at: 50 },
  ]);
});

test('history update: caps at 20, dropping the oldest', () => {
  let hist = [];
  for (let i = 1; i <= 20; i++) hist = updateHistory(hist, `C:\\p${i}`, i);
  assertEqual(hist.length, 20);
  hist = updateHistory(hist, 'C:\\p21', 21);
  assertEqual(hist.length, 20);
  assertEqual(hist[0].path, 'C:\\p21');
  assertEqual(hist.some((h) => h.path === 'C:\\p1'), false, 'oldest should be evicted past the cap');
});

test('history update: does not mutate the input array', () => {
  const input = [{ path: 'C:\\a', at: 1 }];
  const out = updateHistory(input, 'C:\\b', 2);
  assertEqual(input, [{ path: 'C:\\a', at: 1 }], 'original must be untouched');
  assertEqual(out.length, 2);
});

// ---- historyEntryModel -------------------------------------------------------
// Turns a { path, at } history entry into a display model: folder name (tail),
// dim parent path, and a relative last-visited label (reusing the same
// relative-time phrasing as Trash). at=null -> no time label (never visited
// with a known timestamp, e.g. a migrated legacy entry).

test('history model: splits name from parent path', () => {
  const m = historyEntryModel({ path: 'C:\\Users\\thedi\\Desktop\\Work', at: null }, Date.now());
  assertEqual(m.name, 'Work');
  assertEqual(m.parent, 'C:\\Users\\thedi\\Desktop');
  assertEqual(m.path, 'C:\\Users\\thedi\\Desktop\\Work');
});

test('history model: at=null yields an empty relative label', () => {
  const m = historyEntryModel({ path: 'C:\\a\\b', at: null }, Date.now());
  assertEqual(m.ago, '');
});

test('history model: a known timestamp reuses the relative-time phrasing', () => {
  const now = Date.parse('2026-07-05T12:00:00Z');
  const m = historyEntryModel({ path: 'C:\\a\\b', at: now - 5 * 60 * 1000 }, now);
  assertEqual(m.ago, '5m ago');
});

test('history model: a drive root path (no parent) yields an empty parent', () => {
  const m = historyEntryModel({ path: 'C:\\', at: null }, Date.now());
  assertEqual(m.parent, '');
});

// ---- subViewVisibility -------------------------------------------------------
// Pure map from the active Files sub-view ('browser' | 'trash' | 'history') to
// which surfaces are shown. This encodes the show/hide symmetry that a past
// bug got wrong (a sub-view's nodes staying visible after leaving). Each field
// is `true` when that surface should be VISIBLE.

test('view state: browser shows the entry list, hides trash + history', () => {
  const v = subViewVisibility('browser');
  assertEqual(v.entryList, true);
  assertEqual(v.trash, false);
  assertEqual(v.history, false);
});

test('view state: trash shows only the trash surface', () => {
  const v = subViewVisibility('trash');
  assertEqual(v.entryList, false);
  assertEqual(v.trash, true);
  assertEqual(v.history, false);
});

test('view state: history shows only the history surface', () => {
  const v = subViewVisibility('history');
  assertEqual(v.entryList, false);
  assertEqual(v.trash, false);
  assertEqual(v.history, true);
});

test('view state: exactly one primary surface is visible in every view', () => {
  for (const view of ['browser', 'trash', 'history']) {
    const v = subViewVisibility(view);
    const on = [v.entryList, v.trash, v.history].filter(Boolean).length;
    assertEqual(on, 1, `exactly one primary surface should show in '${view}'`);
  }
});

test('view state: an unknown view degrades to the browser (safe default)', () => {
  const v = subViewVisibility('nonsense');
  assertEqual(v.entryList, true);
  assertEqual(v.trash, false);
  assertEqual(v.history, false);
});

// ---- summary ----------------------------------------------------------------

console.log(`\n${passCount} passed, ${failCount} failed`);
if (failCount > 0) {
  process.exit(1);
}
