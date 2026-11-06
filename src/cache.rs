//! Persistent on-disk caches for external-query results.
//!
//! Currently caches `rmadison -u ubuntu <query>` stdout under
//! `$XDG_CACHE_HOME/canonical/thermite/rmadison/` (defaulting to
//! `~/.cache/canonical/thermite/rmadison/`), one file per query. Cached
//! entries make repeated runs fast: a package that was once absent from a
//! release's archive (e.g. a new LLVM in an older LTS) will never appear
//! there, so re-fetching the same "not published" answer adds latency without
//! adding information.
//!
//! The cache is best-effort throughout: lookup, storage, and invalidation
//! failures never fail a workflow — they degrade to running the query.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use tracing::{debug, warn};

use crate::types::params::CacheMode;

/// Namespace directory below the thermite cache base holding rmadison
/// results.
pub const RMADISON_NAMESPACE: &str = "rmadison";

/// Process-global cache mode, mirroring [`crate::shell`]'s verbosity
/// precedent: set once at startup, read anywhere.
static CACHE_MODE: OnceLock<CacheMode> = OnceLock::new();

/// Set the process-wide cache mode.
///
/// Must be called before any cached lookups. Subsequent calls are silently
/// ignored (the mode is immutable after the first call).
pub fn set_cache_mode(mode: CacheMode) {
    let _ = CACHE_MODE.set(mode);
}

/// Returns the current cache mode (defaults to [`CacheMode::On`]).
pub fn cache_mode() -> CacheMode {
    CACHE_MODE.get().copied().unwrap_or_default()
}

/// Activate the CLI `--cache` mode.
///
/// [`CacheMode::Clear`] first wipes the rmadison cache directory, then the
/// effective mode for the rest of the run becomes [`CacheMode::On`]; every
/// other mode is stored as-is.
pub fn activate(mode: CacheMode) {
    if mode == CacheMode::Clear {
        match base_dir() {
            Some(base) => clear_with_mode(CacheMode::On, &base),
            None => warn!("cannot clear cache: neither XDG_CACHE_HOME nor HOME is set"),
        }
        set_cache_mode(CacheMode::On);
    } else {
        set_cache_mode(mode);
    }
}

/// Resolve the thermite cache base directory: `$XDG_CACHE_HOME` when set,
/// otherwise `$HOME/.cache`. Returns `None` when neither is usable.
fn base_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME")
        && !xdg.trim().is_empty()
    {
        return Some(PathBuf::from(xdg).join("canonical").join("thermite"));
    }
    match std::env::var("HOME") {
        Ok(home) if !home.trim().is_empty() => Some(
            PathBuf::from(home)
                .join(".cache")
                .join("canonical")
                .join("thermite"),
        ),
        _ => None,
    }
}

/// A cached rmadison result.
#[derive(Debug, Clone)]
pub struct CacheHit {
    /// The cached stdout of the original `rmadison` invocation.
    pub data: String,
    /// Age of the cache entry in seconds, when its mtime could be read.
    pub age_secs: Option<u64>,
}

/// Validate a cache key (an rmadison query such as `libgit2` or
/// `rustc-1.85`). Keys must be non-empty, free of path separators and
/// traversal segments, and printable ASCII.
fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('.')
        && !key.contains(['/', '\\'])
        && !key.contains("..")
        && key.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// Look up `key` in the rmadison cache, honouring the process cache mode.
///
/// Returns `None` on [`CacheMode::Off`] and [`CacheMode::Update`] (which
/// always refetch), on a cache miss, and on any read error.
pub fn lookup_rmadison(key: &str) -> Option<CacheHit> {
    lookup_with_mode(cache_mode(), &base_dir()?, key)
}

/// Like [`lookup_rmadison`] but against an explicit mode and cache base
/// directory. The testable core.
fn lookup_with_mode(mode: CacheMode, base: &Path, key: &str) -> Option<CacheHit> {
    match mode {
        CacheMode::Off | CacheMode::Update => return None,
        CacheMode::On | CacheMode::Clear => {}
    }
    if !valid_key(key) {
        debug!("rmadison cache: refusing to look up invalid key {key:?}");
        return None;
    }
    let path = entry_path(base, key);
    let data = match std::fs::read_to_string(&path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            debug!(
                "rmadison cache: ignoring unreadable entry {}: {e}",
                path.display()
            );
            return None;
        }
    };
    let age_secs = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|mtime| SystemTime::now().duration_since(mtime).ok())
        .map(|d| d.as_secs());
    debug!("rmadison cache: hit for {key} ({})", format_age(age_secs));
    Some(CacheHit { data, age_secs })
}

/// Store `data` as the rmadison result for `key`, honouring the process cache
/// mode.
///
/// [`CacheMode::Off`] never writes. All other modes write, atomically
/// (temp file + rename). Storage failures are logged and swallowed.
pub fn store_rmadison(key: &str, data: &str) {
    let Some(base) = base_dir() else {
        return;
    };
    store_with_mode(cache_mode(), &base, key, data);
}

/// Like [`store_rmadison`] but against an explicit mode and cache base
/// directory. The testable core.
fn store_with_mode(mode: CacheMode, base: &Path, key: &str, data: &str) {
    match mode {
        CacheMode::Off => return,
        CacheMode::On | CacheMode::Update | CacheMode::Clear => {}
    }
    if !valid_key(key) {
        debug!("rmadison cache: refusing to store invalid key {key:?}");
        return;
    }
    let path = entry_path(base, key);
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        warn!("rmadison cache: cannot create {}: {e}", parent.display());
        return;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, data) {
        warn!("rmadison cache: cannot write {}: {e}", tmp.display());
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        warn!("rmadison cache: cannot finalize {}: {e}", path.display());
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    debug!("rmadison cache: stored {key}");
}

/// Delete the rmadison cache directory, honouring the process cache mode
/// ([`CacheMode::Off`] leaves the cache untouched).
pub fn clear_rmadison() {
    let Some(base) = base_dir() else {
        warn!("cannot clear cache: neither XDG_CACHE_HOME nor HOME is set");
        return;
    };
    clear_with_mode(cache_mode(), &base);
}

/// Like [`clear_rmadison`] but against an explicit mode and cache base
/// directory. The testable core.
fn clear_with_mode(mode: CacheMode, base: &Path) {
    if mode == CacheMode::Off {
        return;
    }
    let dir = namespace_dir(base);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => println!("  Cleared rmadison cache: {}", dir.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!("rmadison cache: cannot clear {}: {e}", dir.display()),
    }
}

/// Path of the cache entry file for `key` under `base`.
fn entry_path(base: &Path, key: &str) -> PathBuf {
    namespace_dir(base).join(format!("{key}.txt"))
}

/// Path of the rmadison namespace directory under `base`.
fn namespace_dir(base: &Path) -> PathBuf {
    base.join(RMADISON_NAMESPACE)
}

/// Format a cache-entry age for the "using cached result" notice.
pub fn format_age(age_secs: Option<u64>) -> String {
    match age_secs {
        None => "age unknown".to_owned(),
        Some(s) if s < 60 => "just now".to_owned(),
        Some(s) if s < 3600 => format!("{}m old", s / 60),
        Some(s) if s < 86400 => format!("{}h old", s / 3600),
        Some(s) => format!("{}d old", s / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn temp_base(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "thermite-cache-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ── valid_key ─────────────────────────────────────────────────────────

    #[test]
    fn valid_key_accepts_typical_queries() {
        assert!(valid_key("libgit2"));
        assert!(valid_key("rustc-1.85"));
        assert!(valid_key("llvm-toolchain-22"));
    }

    #[test]
    fn valid_key_rejects_traversal_and_separators() {
        assert!(!valid_key(""));
        assert!(!valid_key(".hidden"));
        assert!(!valid_key(".."));
        assert!(!valid_key("a/b"));
        assert!(!valid_key("a\\b"));
        assert!(!valid_key("pkg\nname"));
        assert!(!valid_key("pkg name"));
        assert!(!valid_key("pkg name\t"));
    }

    #[test]
    fn valid_key_rejects_non_ascii() {
        assert!(!valid_key("rustc-1.85…"));
    }

    // ── lookup / store round-trip (On mode) ───────────────────────────────

    #[test]
    fn store_then_lookup_round_trips() {
        let base = temp_base("roundtrip");
        store_with_mode(CacheMode::On, &base, "libgit2", "stdout line");
        let hit = lookup_with_mode(CacheMode::On, &base, "libgit2").expect("cache hit");
        assert_eq!(hit.data, "stdout line");
        assert!(hit.age_secs.unwrap_or(u64::MAX) < 60);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn lookup_misses_unknown_key() {
        let base = temp_base("miss");
        assert!(lookup_with_mode(CacheMode::On, &base, "nonesuch").is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn store_creates_missing_namespace_dir() {
        let base = temp_base("mkdirs");
        store_with_mode(CacheMode::On, &base, "pkgconf", "data");
        assert!(entry_path(&base, "pkgconf").is_file());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn store_leaves_no_temp_files_behind() {
        let base = temp_base("tmpclean");
        store_with_mode(CacheMode::On, &base, "cmake", "data");
        let leftovers: Vec<_> = std::fs::read_dir(namespace_dir(&base))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn lookup_treats_corrupt_non_utf8_entry_as_miss() {
        let base = temp_base("corrupt");
        let path = entry_path(&base, "dh-cargo");
        std::fs::create_dir_all(namespace_dir(&base)).unwrap();
        std::fs::write(path, [0xff, 0xfe, 0x00]).unwrap();
        assert!(lookup_with_mode(CacheMode::On, &base, "dh-cargo").is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn invalid_keys_never_touch_the_disk() {
        let base = temp_base("badkey");
        store_with_mode(CacheMode::On, &base, "../escape", "data");
        assert!(lookup_with_mode(CacheMode::On, &base, "../escape").is_none());
        assert!(!base.join("escape.txt").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    // ── mode semantics ────────────────────────────────────────────────────

    #[test]
    fn off_mode_never_reads_or_writes() {
        let base = temp_base("off");
        assert!(lookup_with_mode(CacheMode::Off, &base, "libgit2").is_none());
        store_with_mode(CacheMode::Off, &base, "libgit2", "data");
        assert!(!entry_path(&base, "libgit2").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn update_mode_never_reads_but_writes() {
        let base = temp_base("update");
        store_with_mode(CacheMode::On, &base, "libgit2", "old");
        assert!(lookup_with_mode(CacheMode::Update, &base, "libgit2").is_none());
        store_with_mode(CacheMode::Update, &base, "libgit2", "new");
        assert_eq!(
            std::fs::read_to_string(entry_path(&base, "libgit2")).unwrap(),
            "new"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn update_mode_overwrites_existing_entry() {
        let base = temp_base("overwrite");
        store_with_mode(CacheMode::On, &base, "libgit2", "old");
        store_with_mode(CacheMode::Update, &base, "libgit2", "fresh");
        assert_eq!(
            lookup_with_mode(CacheMode::On, &base, "libgit2")
                .unwrap()
                .data,
            "fresh"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    // ── clear ─────────────────────────────────────────────────────────────

    #[test]
    fn clear_removes_namespace_but_not_the_base() {
        let base = temp_base("clear");
        store_with_mode(CacheMode::On, &base, "libgit2", "data");
        clear_with_mode(CacheMode::On, &base);
        assert!(!namespace_dir(&base).exists());
        assert!(base.exists());
        assert!(lookup_with_mode(CacheMode::On, &base, "libgit2").is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn clear_is_idempotent_when_absent() {
        let base = temp_base("clearabsent");
        clear_with_mode(CacheMode::On, &base);
        assert!(!namespace_dir(&base).exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn clear_in_off_mode_is_a_noop() {
        let base = temp_base("clearoff");
        store_with_mode(CacheMode::On, &base, "libgit2", "data");
        clear_with_mode(CacheMode::Off, &base);
        assert!(entry_path(&base, "libgit2").is_file());
        let _ = std::fs::remove_dir_all(&base);
    }

    // ── format_age ────────────────────────────────────────────────────────

    #[test]
    fn format_age_buckets() {
        assert_eq!(format_age(None), "age unknown");
        assert_eq!(format_age(Some(0)), "just now");
        assert_eq!(format_age(Some(59)), "just now");
        assert_eq!(format_age(Some(60)), "1m old");
        assert_eq!(format_age(Some(3599)), "59m old");
        assert_eq!(format_age(Some(3600)), "1h old");
        assert_eq!(format_age(Some(86399)), "23h old");
        assert_eq!(format_age(Some(86400)), "1d old");
        assert_eq!(format_age(Some(7 * 86400)), "7d old");
    }
}
