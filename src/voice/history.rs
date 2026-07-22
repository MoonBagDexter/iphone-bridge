//! Dictation history: every finished dictation, persisted so it can be
//! searched, re-copied, or re-processed under a different mode.
//!
//! Stored as a JSON array in `%LOCALAPPDATA%\iphone-bridge\history.json`,
//! capped to the newest N entries so it can't grow without bound. Every read
//! and write goes through one process-wide mutex and writes land via a
//! temp-file rename, because dictations finish on `spawn_blocking` threads and
//! two can complete close enough together to interleave.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::logging::log_both;

/// Typical speaking pace, words per minute. Dictating this many words takes
/// roughly a minute; typing the same words takes `TYPING_WPM` long. The gap is
/// what the Home screen reports as time saved. Both figures are the usual
/// rules of thumb, not measured for this user.
const SPEAKING_WPM: f64 = 150.0;
const TYPING_WPM: f64 = 40.0;

/// One finished dictation.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Entry {
    /// Unique and sortable: `<unix secs>-<process salt><sequence>`.
    #[serde(default)]
    pub id: String,
    /// Unix seconds when the dictation finished.
    #[serde(default)]
    pub at: i64,
    /// Mode id the dictation ran under, e.g. "email".
    #[serde(default)]
    pub mode: String,
    /// Audio duration in seconds.
    #[serde(default)]
    pub seconds: f32,
    /// Transcript straight out of whisper, before replacements or AI.
    #[serde(default)]
    pub raw: String,
    /// What was actually delivered. Equals `raw` when nothing post-processed it.
    #[serde(default)]
    pub text: String,
}

impl Entry {
    /// Build an entry stamped with the current time and a fresh id.
    pub fn new(mode: &str, seconds: f32, raw: &str, text: &str) -> Self {
        Entry {
            id: new_id(),
            at: now_secs(),
            mode: mode.to_string(),
            seconds,
            raw: raw.to_string(),
            text: text.to_string(),
        }
    }
}

/// Home-screen counters.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub total: usize,
    pub total_words: usize,
    pub words_this_week: usize,
    /// Minutes saved versus typing the same words by hand.
    pub minutes_saved: f64,
}

/// Append a dictation, then prune to the newest `cap` entries.
pub fn append(mut entry: Entry, cap: usize) -> Result<()> {
    if entry.id.is_empty() {
        entry.id = new_id();
    }
    if entry.at == 0 {
        entry.at = now_secs();
    }

    let _guard = lock();
    let mut all = load_locked();
    all.push(entry);
    // Sorted oldest-first on disk, so pruning is a drain from the front and a
    // backdated entry (a re-import, a clock step) still lands in order.
    sort_oldest_first(&mut all);
    if all.len() > cap {
        all.drain(..all.len() - cap);
    }
    save_locked(&all)
}

/// Every dictation, newest first.
pub fn list() -> Vec<Entry> {
    let _guard = lock();
    let mut all = load_locked();
    sort_oldest_first(&mut all);
    all.reverse();
    all
}

/// Dictations whose raw transcript or delivered text contains `query`,
/// case-insensitively, newest first. An empty query matches everything.
pub fn search(query: &str) -> Vec<Entry> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return list();
    }
    list()
        .into_iter()
        .filter(|e| {
            e.raw.to_lowercase().contains(&needle) || e.text.to_lowercase().contains(&needle)
        })
        .collect()
}

pub fn get(id: &str) -> Option<Entry> {
    let _guard = lock();
    load_locked().into_iter().find(|e| e.id == id)
}

/// Remove one dictation. Removing an id that isn't there is not an error.
pub fn delete(id: &str) -> Result<()> {
    let _guard = lock();
    let mut all = load_locked();
    let before = all.len();
    all.retain(|e| e.id != id);
    if all.len() == before {
        return Ok(());
    }
    save_locked(&all)
}

/// Drop every dictation.
pub fn clear() -> Result<()> {
    let _guard = lock();
    save_locked(&[])
}

pub fn stats() -> Stats {
    let all = {
        let _guard = lock();
        load_locked()
    };

    let week_start = now_secs() - 7 * 24 * 60 * 60;
    let mut total_words = 0usize;
    let mut words_this_week = 0usize;
    for e in &all {
        let words = e.text.split_whitespace().count();
        total_words += words;
        if e.at >= week_start {
            words_this_week += words;
        }
    }

    let w = total_words as f64;
    Stats {
        total: all.len(),
        total_words,
        words_this_week,
        minutes_saved: w / TYPING_WPM - w / SPEAKING_WPM,
    }
}

// ---------------------------------------------------------------- internals

static FILE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Serialize all history file access. The guarded value is `()` -- there is no
/// invariant a panicking holder could leave half-updated (the file itself is
/// only ever replaced atomically), so a poisoned lock is recovered rather than
/// permanently bricking history for the rest of the run.
fn lock() -> MutexGuard<'static, ()> {
    FILE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn history_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(p) = tests::override_dir() {
        return p;
    }
    let base = std::env::var("LOCALAPPDATA").unwrap_or_default();
    PathBuf::from(base).join("iphone-bridge")
}

fn history_path() -> PathBuf {
    history_dir().join("history.json")
}

/// Read and parse the history file. Caller must hold the lock.
///
/// A missing file is simply empty history. A corrupt one is moved aside to
/// `history.corrupt-<secs>.json` and then treated as empty: deleting it would
/// throw away transcripts the user might still want to hand-recover, but
/// leaving it in place would mean either refusing to record future dictations
/// or silently overwriting it on the next append anyway.
fn load_locked() -> Vec<Entry> {
    let path = history_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    match serde_json::from_str::<Vec<Entry>>(&raw) {
        Ok(entries) => entries,
        Err(e) => {
            let aside = history_dir().join(format!("history.corrupt-{}.json", now_secs()));
            let moved = std::fs::rename(&path, &aside);
            log_both(&format!(
                "[history] history.json unreadable ({e}); starting fresh, old file {}",
                match &moved {
                    Ok(_) => format!("kept at {}", aside.display()),
                    Err(err) => format!("could NOT be moved aside: {err}"),
                }
            ));
            Vec::new()
        }
    }
}

/// Replace the history file. Caller must hold the lock.
///
/// Writes a sibling temp file and renames over the target so a crash mid-write
/// leaves the previous history intact rather than a truncated array.
fn save_locked(entries: &[Entry]) -> Result<()> {
    let dir = history_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    let json = serde_json::to_string_pretty(entries).context("serializing history")?;
    let tmp = dir.join(format!(
        "history.json.tmp-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;

    let path = history_path();
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("replacing {}", path.display()));
    }
    Ok(())
}

fn sort_oldest_first(entries: &mut [Entry]) {
    entries.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

static SEQ: AtomicU64 = AtomicU64::new(0);

/// `<unix secs>-<process salt><sequence>`: sorts by time, and stays unique for
/// two dictations in the same second via the counter. The salt keeps a second
/// copy of the bridge (or a restart inside the same second, where the counter
/// is back at zero) from minting the same id.
fn new_id() -> String {
    let salt = *PROCESS_SALT.get_or_init(random_u16);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{:010}-{:04x}{:012x}", now_secs(), salt, seq)
}

static PROCESS_SALT: OnceLock<u16> = OnceLock::new();

/// xorshift64* seeded from the clock and pid, same trick as `random_pin()` in
/// `src/files/config.rs` -- not cryptographic, and doesn't need to be.
fn random_u16() -> u16 {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0) as u64
        ^ (std::process::id() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut x = seed | 1;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 48) as u16
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    // Every public function resolves a fixed path under %LOCALAPPDATA%, so a
    // naive test would clobber the real user's history -- and cargo runs tests
    // as parallel threads in one process, so an env-var override would race.
    // A thread-local redirect gives each test its own private directory with
    // no coordination at all (one test == one thread).
    thread_local! {
        static TEST_DIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    pub(super) fn override_dir() -> Option<PathBuf> {
        TEST_DIR.with(|d| d.borrow().clone())
    }

    /// Redirects history into a fresh temp dir for the life of one test.
    ///
    /// `pub(crate)` because any test that reaches `history::append` transitively --
    /// notably `voice::apply` -- must also be sandboxed, or `cargo test` writes junk
    /// entries into the real user's dictation history.
    pub(crate) struct Sandbox(PathBuf);

    impl Sandbox {
        pub(crate) fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "iphone-bridge-history-test-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create sandbox");
            TEST_DIR.with(|d| *d.borrow_mut() = Some(dir.clone()));
            Sandbox(dir)
        }

        fn file(&self) -> PathBuf {
            self.0.join("history.json")
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            TEST_DIR.with(|d| *d.borrow_mut() = None);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An entry at an explicit time, so ordering and the week boundary can be
    /// tested without sleeping.
    fn at(secs: i64, text: &str) -> Entry {
        let mut e = Entry::new("default", 1.0, text, text);
        e.at = secs;
        e
    }

    #[test]
    fn append_then_list_roundtrips_every_field() {
        let _s = Sandbox::new("roundtrip");
        let mut e = Entry::new("email", 12.5, "raw words", "Polished words.");
        e.at = 1_700_000_000;
        append(e, 50).expect("append");

        let all = list();
        assert_eq!(all.len(), 1, "one append must yield one entry");
        assert_eq!(all[0].mode, "email");
        assert_eq!(all[0].seconds, 12.5);
        assert_eq!(all[0].raw, "raw words");
        assert_eq!(all[0].text, "Polished words.");
        assert_eq!(all[0].at, 1_700_000_000);
        assert!(!all[0].id.is_empty(), "append must assign an id");
    }

    #[test]
    fn list_returns_newest_first() {
        let _s = Sandbox::new("ordering");
        for (t, txt) in [(100, "oldest"), (300, "newest"), (200, "middle")] {
            append(at(t, txt), 50).expect("append");
        }
        let texts: Vec<String> = list().into_iter().map(|e| e.text).collect();
        assert_eq!(
            texts,
            vec!["newest", "middle", "oldest"],
            "list must be newest-first regardless of insertion order"
        );
    }

    #[test]
    fn cap_prunes_the_oldest_and_keeps_the_newest_n() {
        let _s = Sandbox::new("cap");
        for i in 0..10 {
            append(at(1000 + i, &format!("entry {i}")), 3).expect("append");
        }
        let all = list();
        assert_eq!(all.len(), 3, "cap of 3 must leave exactly 3 entries");
        let texts: Vec<String> = all.into_iter().map(|e| e.text).collect();
        assert_eq!(
            texts,
            vec!["entry 9", "entry 8", "entry 7"],
            "pruning must drop the oldest, not the newest"
        );
    }

    #[test]
    fn missing_file_is_empty_not_an_error() {
        let _s = Sandbox::new("missing");
        assert!(list().is_empty(), "no file means no history");
        assert!(search("anything").is_empty());
        assert!(get("nope").is_none());
        assert_eq!(stats().total, 0);
    }

    #[test]
    fn corrupt_file_is_kept_aside_and_appends_still_work() {
        let s = Sandbox::new("corrupt");
        std::fs::write(s.file(), "{ this is not a json array").expect("write junk");

        assert!(list().is_empty(), "corrupt history must read as empty");

        append(at(500, "after the corruption"), 50).expect("append must survive a corrupt file");
        let all = list();
        assert_eq!(all.len(), 1, "a dictation after corruption must not be lost");
        assert_eq!(all[0].text, "after the corruption");

        let kept: Vec<_> = std::fs::read_dir(&s.0)
            .expect("read sandbox")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("history.corrupt-"))
            .collect();
        assert_eq!(kept.len(), 1, "the corrupt file must be preserved, not deleted");
    }

    #[test]
    fn search_is_case_insensitive_and_matches_mid_string() {
        let _s = Sandbox::new("search-case");
        append(at(100, "Send the QUARTERLY report to Dave"), 50).expect("append");
        append(at(200, "unrelated chatter"), 50).expect("append");

        assert_eq!(search("quarterly").len(), 1, "lowercase query must match uppercase text");
        assert_eq!(search("QUARTERLY").len(), 1, "uppercase query must match too");
        assert_eq!(
            search("rterly rep").len(),
            1,
            "a match in the middle of the text must count"
        );
    }

    #[test]
    fn search_matches_raw_as_well_as_delivered_text() {
        let _s = Sandbox::new("search-raw");
        let mut e = Entry::new("email", 1.0, "um send it to dave", "Please send it to Dave.");
        e.at = 100;
        append(e, 50).expect("append");

        assert_eq!(search("um send").len(), 1, "the raw transcript must be searchable");
        assert_eq!(search("Please send").len(), 1, "the delivered text must be searchable");
    }

    #[test]
    fn search_with_no_match_returns_empty() {
        let _s = Sandbox::new("search-none");
        append(at(100, "hello world"), 50).expect("append");
        assert!(
            search("zebra").is_empty(),
            "a query matching nothing must return no entries"
        );
    }

    #[test]
    fn search_results_are_newest_first() {
        let _s = Sandbox::new("search-order");
        append(at(100, "meeting notes one"), 50).expect("append");
        append(at(200, "meeting notes two"), 50).expect("append");
        let texts: Vec<String> = search("meeting").into_iter().map(|e| e.text).collect();
        assert_eq!(texts, vec!["meeting notes two", "meeting notes one"]);
    }

    #[test]
    fn get_and_delete_work_by_id() {
        let _s = Sandbox::new("get-delete");
        append(at(100, "keep me"), 50).expect("append");
        append(at(200, "delete me"), 50).expect("append");

        let target = list()
            .into_iter()
            .find(|e| e.text == "delete me")
            .expect("entry must exist");
        assert_eq!(
            get(&target.id).map(|e| e.text),
            Some("delete me".to_string()),
            "get must find an entry by its id"
        );

        delete(&target.id).expect("delete");
        assert!(get(&target.id).is_none(), "deleted entry must be gone");
        assert_eq!(list().len(), 1, "delete must remove exactly one entry");
    }

    #[test]
    fn deleting_an_unknown_id_is_a_no_op() {
        let _s = Sandbox::new("delete-missing");
        append(at(100, "still here"), 50).expect("append");
        delete("not-a-real-id").expect("deleting a missing id must not error");
        assert_eq!(list().len(), 1, "nothing must be removed");
    }

    #[test]
    fn clear_empties_the_history() {
        let _s = Sandbox::new("clear");
        for i in 0..5 {
            append(at(100 + i, "something"), 50).expect("append");
        }
        clear().expect("clear");
        assert!(list().is_empty(), "clear must leave no entries");
        // And the file must still be usable afterwards.
        append(at(999, "after clear"), 50).expect("append");
        assert_eq!(list().len(), 1);
    }

    #[test]
    fn stats_count_words_and_estimate_time_saved() {
        let _s = Sandbox::new("stats-words");
        // 40 words total: one minute of typing, 40/150 of a minute of speaking.
        for _ in 0..10 {
            append(at(now_secs(), "one two three four"), 50).expect("append");
        }
        let s = stats();
        assert_eq!(s.total, 10, "one entry per append");
        assert_eq!(s.total_words, 40, "four words per entry");
        let expected = 40.0 / TYPING_WPM - 40.0 / SPEAKING_WPM;
        assert!(
            (s.minutes_saved - expected).abs() < 1e-9,
            "time saved must be words/typing_wpm - words/speaking_wpm, got {}",
            s.minutes_saved
        );
    }

    #[test]
    fn stats_this_week_excludes_older_entries() {
        let _s = Sandbox::new("stats-week");
        let now = now_secs();
        let day = 24 * 60 * 60;
        append(at(now - day, "inside the window"), 50).expect("append"); // 3 words
        append(at(now - 8 * day, "outside of the window here"), 50).expect("append"); // 5 words

        let s = stats();
        assert_eq!(s.total_words, 8, "all entries count towards the total");
        assert_eq!(
            s.words_this_week, 3,
            "only the last 7 days count towards this week"
        );
    }

    #[test]
    fn two_appends_in_the_same_second_get_distinct_ids() {
        let _s = Sandbox::new("ids");
        for _ in 0..50 {
            append(Entry::new("default", 1.0, "x", "x"), 100).expect("append");
        }
        let ids: std::collections::HashSet<String> = list().into_iter().map(|e| e.id).collect();
        assert_eq!(
            ids.len(),
            50,
            "ids must stay unique even when many land in the same second"
        );
    }

    #[test]
    fn ids_sort_in_chronological_order() {
        let _s = Sandbox::new("id-sort");
        let a = Entry::new("default", 1.0, "first", "first");
        let b = Entry::new("default", 1.0, "second", "second");
        assert!(a.id < b.id, "ids minted later must sort after earlier ones");
    }

    #[test]
    fn write_leaves_no_temp_files_behind() {
        let s = Sandbox::new("tmp-cleanup");
        append(at(100, "hello"), 50).expect("append");
        let leftovers: Vec<_> = std::fs::read_dir(&s.0)
            .expect("read sandbox")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "the temp file must be renamed away");
    }
}
