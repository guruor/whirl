//! `cache/index.json` and the sweep: docs/spec/state-and-cache.md 2.1 and 5.5.
//!
//! 7.2's ownership table puts both halves of this module in the daemon's column:
//! `cache/index.json` is "daemon" for writing and for reading, and the sweep is
//! "the daemon, for a sweep". 5.5 is the one implementation of that sweep, with
//! three trigger points and one rule that shapes everything here: it "always runs
//! while holding the rotation lock", which is what makes it unable to race a
//! download. The lock itself is `crate::lock::take_rotate`; the three triggers
//! are in `crate::state::Daemon::sweep` and `crate::state::Daemon::finish_rotation`.
//!
//! **What this module does not touch.** The sweep removes only three kinds of
//! file, and every one of them is under the cache root:
//!
//! - a `sha256/` file the index does not know and that is past
//!   `cache.grace_seconds` (5.5 step 3, [`sweep`]);
//! - a `tmp/*.part` file past `cache.orphan_grace_seconds` (5.5 step 4);
//! - an unprotected index entry's file, oldest `last_used` first, and only while
//!   a cap is exceeded (5.5 step 5).
//!
//! It never touches the anchor's file (INV-CACHE-2), anything pinned by
//! `favorites.json` or by the index's own `pinned` copy (INV-CACHE-3), a file
//! created inside `cache.grace_seconds` (5.3), or anything at all outside the
//! cache root. It never raises a cap, and it never removes a file to "make room"
//! for something else.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use whirl_core::protocol::Kind;
use whirl_core::state::{CacheIndexEntry, INDEX_FILE, IndexFile, StateFile, content_path, tmp_dir};

use crate::statefile;

/// 5.4's four causes of a surplus, and the whole list: `status` reports a
/// non-zero `cache_over_cap` only with one of these names beside it, because
/// "a sweep that cannot run, or a pin set that cannot be read, is reported as
/// one of them rather than silently exempted".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// One protected file is larger than `cache.max_bytes` on its own (5.2 case
    /// 2: reachable only by a file dropped into the cache by hand).
    SingleFile,
    /// The protected set on its own exceeds a cap (5.3).
    Pinned,
    /// The sweep could not enforce the bound: it failed, it could not take the
    /// rotation lock, or it has not run yet. 8.1 gives the disk-full case the
    /// same answer: "a sweep that cannot rewrite `index.json` reports
    /// `cache_over_reason: sweep_error` and retries at the next rotation".
    SweepError,
    /// `favorites.json` is quarantined, so the pin set cannot be read and the
    /// whole cache is protected (5.3, 6.4).
    FavoritesDegraded,
}

impl Cause {
    /// The `cache_over_reason` value of 2.10. One place, so the key and the log
    /// line cannot disagree about the name.
    pub fn as_str(self) -> &'static str {
        match self {
            Cause::SingleFile => "single_file",
            Cause::Pinned => "pinned",
            Cause::SweepError => "sweep_error",
            Cause::FavoritesDegraded => "favorites_degraded",
        }
    }
}

/// What happened to the last sweep attempt. 2.10 gives `sweep_deferred` exactly
/// one meaning, "the rotation lock was already held by someone else", and 5.4
/// gives the same attempt a second reading as one of its four causes; one value
/// carries both, so the two keys cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    /// No sweep has run in this daemon's lifetime yet.
    Pending,
    /// A sweep ran, steps 2 to 6 of 5.5.
    Ran,
    /// 5.5 step 1: the rotation lock was held by someone else, so the sweep
    /// returned without doing anything and the next rotation retries.
    Deferred,
    /// A sweep ran and could not finish; the message is in the log (5.4).
    Failed,
}

impl Attempt {
    /// 2.10's `sweep_deferred`: 1 for the one case 5.5 step 1 describes, "the
    /// rotation lock was already held by someone else".
    pub fn deferred(self) -> bool {
        self == Attempt::Deferred
    }
}

/// Why 5.3 keeps a file, when it does. The rule matters as well as the fact:
/// 2.9's `cache_swept` reports how many entries were kept "only because they are
/// pinned".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    /// The file `current.json`'s anchor names (INV-CACHE-2).
    Anchor,
    /// A `favorites.json` pin, or the index's own `pinned` copy of it, or the
    /// whole cache while the pin set is unreadable (INV-CACHE-3).
    Pinned,
    /// Created inside `cache.grace_seconds` (5.3).
    Grace,
}

/// 5.3's protected set, resolved against the state files the daemon holds.
///
/// Two keys per rule, a digest and a path, because the two can be the only one
/// available: an index entry carries the digest, and a `Favorite` carries both,
/// while a file the daemon died before recording carries neither in the index
/// and only its path in `current.json` or `favorites.json`.
#[derive(Debug, Default, Clone)]
pub struct Protected {
    pub anchor_digest: Option<String>,
    pub anchor_paths: BTreeSet<String>,
    pub pinned_digests: BTreeSet<String>,
    pub pinned_paths: BTreeSet<String>,
    /// 5.3's escalation: while `favorites_degraded: 1` (6.4) the protected set is
    /// the whole cache, and `tmp/` is still cleaned because a part file is never
    /// a pin.
    pub degraded: bool,
}

impl Protected {
    /// Does the sweep keep this file, and under which rule (5.3)?
    pub fn keeps(&self, file: &Cached, now: i64, grace_seconds: u64) -> Option<Kept> {
        if self.degraded {
            return Some(Kept::Pinned);
        }
        if let Some(anchor) = &self.anchor_digest {
            if file.digest.as_deref() == Some(anchor.as_str()) {
                return Some(Kept::Anchor);
            }
        }
        let path = file.path.to_string_lossy();
        if self.anchor_paths.contains(path.as_ref()) {
            return Some(Kept::Anchor);
        }
        if self.pinned_paths.contains(path.as_ref()) {
            return Some(Kept::Pinned);
        }
        if let Some(digest) = &file.digest {
            if self.pinned_digests.contains(digest) {
                return Some(Kept::Pinned);
            }
        }
        match file.mtime {
            // 5.3: "any cache file created within `cache.grace_seconds` (default
            // 600) in case a second process, such as a hand-run worker, is
            // between rename and report". This is the rule 5.4's step 4 has to
            // wait out, and the reason it is stated in wall-clock seconds.
            Some(modified) if now - modified < grace_seconds as i64 => Some(Kept::Grace),
            // An age this daemon cannot read is not an age it guesses at: the
            // file could be exactly the in-flight one the grace exists for.
            None => Some(Kept::Grace),
            Some(_) => None,
        }
    }
}

/// One file under `sha256/`, measured (5.5 step 3 and step 5 both need the size
/// and the age, and step 3 needs to know whether the index knows it).
#[derive(Debug, Clone)]
pub struct Cached {
    pub path: PathBuf,
    /// The 64-hex digest the file name carries, when it is one.
    pub digest: Option<String>,
    pub bytes: u64,
    /// The file's own `mtime`, in unix seconds: what "created within
    /// `cache.grace_seconds`" is measured against, since a rename does not touch
    /// it (5.3).
    pub mtime: Option<i64>,
}

/// The cache's images, measured: `sha256/` and nothing else.
///
/// 5.4's check is the definition of this set: `du -sk "$CACHE/sha256"` against
/// `cache.max_bytes` and `find "$CACHE/sha256" -type f | wc -l` against
/// `cache.max_files`. `tmp/` is the sweep's own separate step (5.5 step 4) and
/// `index.json` is metadata, not an image, so neither counts toward a cap; the
/// same measurement is what 2.10's `cache_files` and `cache_bytes` report and
/// what 5.5 step 7's `files=` and `bytes=` print, which is why the two carry the
/// same pair of numbers.
#[derive(Debug, Default, Clone)]
pub struct Survey {
    pub files: u64,
    pub bytes: u64,
    pub cached: BTreeMap<PathBuf, Cached>,
}

impl Survey {
    fn forget(&mut self, path: &Path) -> Option<Cached> {
        let gone = self.cached.remove(path)?;
        self.files = self.files.saturating_sub(1);
        self.bytes = self.bytes.saturating_sub(gone.bytes);
        Some(gone)
    }

    /// The protected files, with their sizes: 5.4's `single_file` cause asks
    /// whether this set is one file larger than the byte cap.
    fn kept(&self, protected: &Protected, now: i64, grace_seconds: u64) -> Vec<(&Cached, Kept)> {
        self.cached
            .values()
            .filter_map(|file| {
                protected
                    .keeps(file, now, grace_seconds)
                    .map(|kept| (file, kept))
            })
            .collect()
    }
}

/// One rotation's report, as 7.3 step 4 needs it: the daemon is the writer of
/// the index entry, not the worker.
#[derive(Debug, Clone)]
pub struct Reported {
    pub digest: String,
    pub origin_key: String,
    /// The absolute path the worker reported (1.6's `set:` line).
    pub path: String,
    /// The origin's kind, from the `origin_key` prefix (2.6).
    pub kind: Kind,
}

/// What one sweep did, for the log line of 5.5 step 7, the `cache_swept` event
/// of 2.9, and `status` (5.4).
#[derive(Debug, Clone)]
pub struct Swept {
    /// 5.5 step 7: the two numbers `status` also reports, measured after the
    /// sweep.
    pub files: u64,
    pub bytes: u64,
    /// 5.5 step 5: index entries evicted, oldest `last_used` first.
    pub removed: u64,
    /// 5.5 step 3: files under `sha256/` the index did not know, reclaimed.
    pub reclaimed: u64,
    /// 5.5 step 4: `tmp/*.part` files past `cache.orphan_grace_seconds`.
    pub orphans: u64,
    /// Every byte this sweep freed, across steps 3, 4 and 5 (2.9's
    /// `cache_swept <removed> <reclaimed_bytes> <hidden>`).
    pub freed_bytes: u64,
    /// 2.9's `hidden`: entries kept at the end only because they are pinned.
    pub hidden: u64,
}

/// Read `cache/index.json` (2.1).
///
/// A cache directory with no index yet is not an error and not an identity: the
/// returned `root_id` is empty until a writer mints it (see [`mint_if_absent`]),
/// which is what keeps "minted when the cache directory is created" true for one
/// cache rather than for every reader of it. A file that cannot be read or parsed
/// is an error rather than an empty index: 6.4's quarantine-then-rebuild is
/// written for the three state files, and for the cache the cheap mistake would
/// be to treat every image as an orphan and reclaim the whole cache. A file whose
/// `schema` is newer than this build is the same non-event as 6.4 step 4: it is
/// left exactly as it is. Both cases land in 5.4's `sweep_error`, with the message
/// in the log.
pub fn load(root: &Path, now: i64) -> Result<IndexFile, String> {
    let path = root.join(INDEX_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(IndexFile {
                seq: 0,
                written_at: whirl_core::protocol::rfc3339_utc(now),
                root_id: String::new(),
                entries: BTreeMap::new(),
                dangling: Vec::new(),
            });
        }
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    match IndexFile::parse(&text) {
        Ok(StateFile::Read(index)) => Ok(index),
        Ok(StateFile::SchemaNewer { found }) => Err(format!(
            "{} is schema {found}, newer than this build's; left as it is",
            path.display()
        )),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// 2.1: "`root_id` is a UUID minted when the cache directory is created." The mint
/// happens in one function and on the way to a write, so the identity a cache
/// carries on disk is the one every reader of it sees: a cache whose directory was
/// deleted and re-created (8.2) gets a new id at the next write, which is exactly
/// what tells a state file that the bytes it remembers are not there any more.
///
/// `true` when this call minted it.
fn mint_if_absent(index: &mut IndexFile, now: i64) -> bool {
    if !index.root_id.is_empty() {
        return false;
    }
    index.root_id = mint_root_id(now);
    true
}

/// 5.5 step 6: write `index.json` atomically, "even if nothing changed, so that
/// `seq` and `written_at` are honest about the last time the sweep ran". The
/// protocol is 6.3's, in the cache root instead of the state directory, because
/// the worker and the next daemon start both read this file.
pub fn write(root: &Path, index: &IndexFile) -> Result<(), String> {
    statefile::write_named(root, INDEX_FILE, &index.encode())
}

/// The two directions of 5.5 step 3, plus the measured tree both need: every
/// file under `sha256/` with its size and its age.
///
/// A missing `sha256/` is an empty cache, not an error: nothing has been
/// downloaded yet. A directory that exists and cannot be read is an error, and
/// the sweep that asked turns it into `sweep_error` rather than into a cache
/// that looks empty.
pub fn survey(root: &Path) -> Result<Survey, String> {
    let mut survey = Survey::default();
    let images = root.join("sha256");
    match std::fs::read_dir(&images) {
        Ok(_) => walk(&images, &mut survey)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot read {}: {error}", images.display())),
    }
    Ok(survey)
}

fn walk(directory: &Path, survey: &mut Survey) -> Result<(), String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
        let path = entry.path();
        let metadata = entry
            .metadata()
            .map_err(|error| format!("cannot stat {}: {error}", path.display()))?;
        if metadata.is_dir() {
            walk(&path, survey)?;
        } else if metadata.is_file() {
            survey.files += 1;
            survey.bytes += metadata.len();
            survey.cached.insert(
                path.clone(),
                Cached {
                    digest: digest_of(&path),
                    bytes: metadata.len(),
                    mtime: modified(&metadata),
                    path,
                },
            );
        }
    }
    Ok(())
}

/// `sha256/aa/bb/<64 hex>.<ext>`: the digest the file name carries, when it is
/// one. A file with another name is still a file under `sha256/`, so it is
/// measured and still never evicted by anything but 5.5 step 3; it just cannot
/// be recognized as an index entry's file.
fn digest_of(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name.split('.').next()?;
    let hex = stem.len() == 64 && stem.bytes().all(|byte| byte.is_ascii_hexdigit());
    hex.then(|| stem.to_ascii_lowercase())
}

fn modified(metadata: &std::fs::Metadata) -> Option<i64> {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_secs() as i64)
}

/// 5.4's classification of a surviving surplus: is the protected set one file
/// larger than the byte cap (`single_file`, 5.2 case 2), or is it the set itself
/// (`pinned`, 5.3)? `None` when both caps hold.
///
/// This is the answer for the state a completed sweep leaves behind, which is
/// the only state 5.4 describes: after step 5, a cache that is still over a cap
/// has no unprotected index entry left, so what remains is the protected set.
pub fn over_cause(
    survey: &Survey,
    max_bytes: u64,
    max_files: u64,
    protected: &Protected,
    now: i64,
    grace_seconds: u64,
) -> Option<Cause> {
    if survey.bytes <= max_bytes && survey.files <= max_files {
        return None;
    }
    let kept = survey.kept(protected, now, grace_seconds);
    match kept.as_slice() {
        [(file, _)] if file.bytes > max_bytes => Some(Cause::SingleFile),
        _ => Some(Cause::Pinned),
    }
}

/// 7.3 step 4: write the index entry for the file a rotation reported, and bump
/// its `last_used` on a cache hit (3 step 7: "the daemon bumps `last_used`").
///
/// `Ok(None)` is the two cases where there is nothing to record and no fault: a
/// reference-mode set, whose path is the user's own file outside the cache root,
/// and a byte-for-byte duplicate the index already knows. The daemon is the
/// writer of this file (7.2) and the only one; `first_seen` is the one field the
/// index keeps from the first write.
pub fn record(
    root: &Path,
    reported: &Reported,
    protected: &Protected,
    now: i64,
) -> Result<Option<CacheIndexEntry>, String> {
    let path = PathBuf::from(&reported.path);
    let images = root.join("sha256");
    if !path.starts_with(&images) {
        return Ok(None);
    }
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => metadata,
        // 7.3 step 4 has the daemon validate that the reported path "exists"; a
        // file that does not is not recorded, and the next sweep is what
        // reconciles it.
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot stat {}: {error}", path.display())),
    };
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_string();
    let source = reported
        .origin_key
        .split_once(':')
        .map(|(prefix, _)| prefix.to_string())
        .unwrap_or_else(|| reported.origin_key.clone());
    let mut index = load(root, now)?;
    mint_if_absent(&mut index, now);
    let written_at = whirl_core::protocol::rfc3339_utc(now);
    let entry = index
        .entries
        .entry(reported.digest.clone())
        .or_insert_with(|| CacheIndexEntry {
            ext,
            bytes: metadata.len(),
            first_seen: written_at.clone(),
            last_used: written_at.clone(),
            source,
            kind: reported.kind,
            // 2.1: `origin` comes from the bytes, and the daemon holds none
            // (1.9); the worker's `set:` line (1.6) carries the origin key and
            // the cache path and no origin, so this is written `null`.
            origin: None,
            origin_key: reported.origin_key.clone(),
            width: None,
            height: None,
            pinned: protected.pinned_digests.contains(&reported.digest),
        });
    entry.bytes = metadata.len();
    entry.last_used = written_at.clone();
    entry.pinned = protected.pinned_digests.contains(&reported.digest);
    index.seq += 1;
    index.written_at = written_at;
    // A digest the index had already moved to `dangling` is back, so it stops
    // being an entry `status` should report as one whose file is missing.
    index.dangling.retain(|digest| digest != &reported.digest);
    let entry = entry.clone();
    write(root, &index)?;
    Ok(Some(entry))
}

/// 5.5 steps 2 to 6, the whole sweep, in 5.5's order. Step 1 (the lock) and step
/// 7 (the log line and the release) are the caller's: `crate::state::Daemon::sweep`.
pub fn sweep(
    root: &Path,
    config: &CacheConfig,
    protected: &Protected,
    now: i64,
) -> Result<Swept, String> {
    // Step 2: re-read `index.json`.
    let mut index = load(root, now)?;

    // Step 3, first direction: an index entry whose file is missing moves to
    // `dangling`. The set of paths the index claims is what the second direction
    // needs, so the two are one walk over one measurement.
    let mut measured = survey(root)?;
    let mut known: BTreeSet<PathBuf> = BTreeSet::new();
    let mut kept_entries: BTreeMap<String, CacheIndexEntry> = BTreeMap::new();
    let mut dangling: Vec<String> = Vec::new();
    for (digest, entry) in std::mem::take(&mut index.entries) {
        let path = content_path(root, &digest, &entry.ext);
        if measured.cached.contains_key(&path) {
            known.insert(path);
            kept_entries.insert(digest, entry);
        } else {
            // 2.1: "a small list of entries whose file is missing, kept so that
            // `whirl status` can report them before the next sweep reclaims
            // them".
            dangling.push(digest);
        }
    }
    index.entries = kept_entries;
    dangling.sort();
    index.dangling = dangling;

    // Step 3, second direction: a file under `sha256/` with no index entry is an
    // orphan, removed unless it is inside `cache.grace_seconds` (a hand-run
    // worker between rename and report is the only case).
    let orphans: Vec<PathBuf> = measured
        .cached
        .keys()
        .filter(|path| !known.contains(*path))
        .cloned()
        .collect();
    let mut reclaimed = 0u64;
    let mut freed_bytes = 0u64;
    for path in orphans {
        let file = measured
            .cached
            .get(&path)
            .cloned()
            .expect("a path the measurement just listed");
        if protected.keeps(&file, now, config.grace_seconds).is_some() {
            continue;
        }
        remove_file(&path)?;
        measured.forget(&path);
        reclaimed += 1;
        freed_bytes += file.bytes;
    }

    // Step 4: `tmp/` entries older than `cache.orphan_grace_seconds`, "whatever
    // they are, because a part file has no other owner". `tmp/` is still cleaned
    // while the pin set is degraded, because a part file is never a pin (5.3).
    let mut parts = 0u64;
    for (path, bytes) in stale_parts(root, now, config.orphan_grace_seconds)? {
        remove_file(&path)?;
        parts += 1;
        freed_bytes += bytes;
    }

    // Step 5: while over either cap, take the unprotected entry with the oldest
    // `last_used` and remove its file and its index entry. Stop when both caps
    // are satisfied, or when no unprotected entry remains (the protected set
    // alone exceeds a cap; 5.3), whichever comes first.
    let mut removed = 0u64;
    loop {
        if measured.bytes <= config.max_bytes && measured.files <= config.max_files {
            break;
        }
        let victim = index
            .entries
            .iter()
            .filter_map(|(digest, entry)| {
                let path = content_path(root, digest, &entry.ext);
                let file = measured.cached.get(&path)?;
                if protected.keeps(file, now, config.grace_seconds).is_some() {
                    return None;
                }
                // 2.1: `last_used` is the eviction key. RFC 3339 UTC sorts
                // lexicographically, and the digest breaks a tie, so the choice
                // is total and the same on every run.
                Some((entry.last_used.clone(), digest.clone(), path, file.bytes))
            })
            .min();
        let Some((_, digest, path, bytes)) = victim else {
            break;
        };
        remove_file(&path)?;
        measured.forget(&path);
        index.entries.remove(&digest);
        removed += 1;
        freed_bytes += bytes;
    }

    // The index's `pinned` copy is refreshed from the authoritative pin set on
    // every sweep, so a hand-edited file heals at the next one (2.1). The
    // protection above is the union either way, and the index's own `pinned` is
    // never consulted as a *removal* argument, only as a keep.
    for (digest, entry) in index.entries.iter_mut() {
        entry.pinned = protected.pinned_digests.contains(digest);
    }

    let over = over_cause(
        &measured,
        config.max_bytes,
        config.max_files,
        protected,
        now,
        config.grace_seconds,
    );

    // Step 6: write it even if nothing changed.
    mint_if_absent(&mut index, now);
    index.seq += 1;
    index.written_at = whirl_core::protocol::rfc3339_utc(now);
    let hidden = if over.is_some() {
        measured
            .kept(protected, now, config.grace_seconds)
            .into_iter()
            .filter(|(_, kept)| *kept == Kept::Pinned)
            .count() as u64
    } else {
        0
    };
    write(root, &index)?;

    Ok(Swept {
        files: measured.files,
        bytes: measured.bytes,
        removed,
        reclaimed,
        orphans: parts,
        freed_bytes,
        hidden,
    })
}

/// The caps and the two graces, as 5.1 and 5.3 define them. A borrow of
/// `Config::cache`, so a sweep reads the numbers the daemon started with.
pub struct CacheConfig {
    pub max_bytes: u64,
    pub max_files: u64,
    pub grace_seconds: u64,
    pub orphan_grace_seconds: u64,
}

impl From<&whirl_core::config::Cache> for CacheConfig {
    fn from(config: &whirl_core::config::Cache) -> CacheConfig {
        CacheConfig {
            max_bytes: config.max_bytes,
            max_files: config.max_files,
            grace_seconds: config.grace_seconds,
            orphan_grace_seconds: config.orphan_grace_seconds,
        }
    }
}

/// 5.5 step 4's list: every file under `tmp/` older than
/// `cache.orphan_grace_seconds`, with its size. A `tmp/` that does not exist is
/// an empty list, and a `tmp/` that cannot be read is an error the sweep reports
/// rather than a directory it skips.
fn stale_parts(
    root: &Path,
    now: i64,
    orphan_grace_seconds: u64,
) -> Result<Vec<(PathBuf, u64)>, String> {
    let mut stale = Vec::new();
    let mut pending = vec![tmp_dir(root)];
    while let Some(directory) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!("cannot read {}: {error}", directory.display()));
            }
        };
        for entry in entries {
            let entry =
                entry.map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
            let path = entry.path();
            let metadata = entry
                .metadata()
                .map_err(|error| format!("cannot stat {}: {error}", path.display()))?;
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            let age = modified(&metadata);
            // A file this daemon cannot date is left alone: deleting the wrong
            // part file is a failed download, not a reclaimed one.
            let stale_by = match age {
                Some(modified) => now - modified,
                None => 0,
            };
            if stale_by >= orphan_grace_seconds as i64 {
                stale.push((path, metadata.len()));
            }
        }
    }
    Ok(stale)
}

fn remove_file(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot remove {}: {error}", path.display())),
    }
}

/// 2.1's `root_id`: "a UUID minted when the cache directory is created", so that
/// a state file that outlives the cache can tell "the cache was cleared" from
/// "the cache is a different cache". Version 4 of RFC 4122 [11] from the
/// system's entropy source, with no third-party dependency (the workspace has
/// none): `/dev/urandom` where it exists, and the clock, the pid and an address
/// where it does not, which is still unique per cache directory.
fn mint_root_id(now: i64) -> String {
    let mut bytes = [0u8; 16];
    let filled = std::fs::File::open("/dev/urandom")
        .and_then(|mut source| std::io::Read::read_exact(&mut source, &mut bytes))
        .is_ok();
    if !filled {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.subsec_nanos())
            .unwrap_or(0);
        let mut seed = (now as u64) ^ ((std::process::id() as u64) << 32) ^ (nanos as u64);
        for byte in bytes.iter_mut() {
            // xorshift64: enough for an identifier two daemons compare, and
            // nothing is derived from it.
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *byte = (seed & 0xff) as u8;
        }
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::unix_seconds;

    /// A 64-hex digest of one repeated character: the file name 2.1's layout gives
    /// a cache file, so `content_path` puts a planted file two levels down exactly
    /// where the sweep looks for it, and `digest_of` reads it back.
    fn digest(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    /// One scratch cache root per test, named for the test.
    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("whirl-cache-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch cache root");
        root
    }

    /// The caps and the graces, with the ones a test drives supplied.
    ///
    /// 5.4's checks are stated at `cache.max_files` 1 -- "never below 1" -- while
    /// a daemon can be configured only with 2 or more (4.3, `validate_ordering`),
    /// so the invariants are exercised here at the count cap the sweep itself
    /// implements, and the daemon-level check runs at the tightest legal 2.
    fn config(max_files: u64, max_bytes: u64, grace_seconds: u64) -> CacheConfig {
        CacheConfig {
            max_bytes,
            max_files,
            grace_seconds,
            orphan_grace_seconds: 300,
        }
    }

    /// One cache file at the path 2.1's layout gives it, filled with `bytes` of
    /// filler so a test can drive the byte cap as well as the count cap.
    fn plant(root: &Path, digest: &str, ext: &str, bytes: usize) -> PathBuf {
        let path = content_path(root, digest, ext);
        std::fs::create_dir_all(path.parent().expect("a digest directory")).expect("the parents");
        std::fs::write(&path, vec![b'x'; bytes]).expect("a cache file");
        path
    }

    /// `index.json` with one entry per `(digest, last_used)` pair, in the order
    /// given. `last_used` is 2.1's eviction key and the only thing 5.5 step 5
    /// orders by; the entry's own `bytes` is what 2.1 records, not what the sweep
    /// measures, and the sweep measures the file.
    fn plant_index(root: &Path, entries: &[(&str, &str)]) {
        let mut index = IndexFile {
            seq: 0,
            written_at: "2026-01-01T00:00:00Z".to_string(),
            root_id: "test-root".to_string(),
            entries: BTreeMap::new(),
            dangling: Vec::new(),
        };
        for (digest, last_used) in entries {
            index.entries.insert(
                (*digest).to_string(),
                CacheIndexEntry {
                    ext: "jpg".to_string(),
                    bytes: 0,
                    first_seen: "2026-01-01T00:00:00Z".to_string(),
                    last_used: (*last_used).to_string(),
                    source: "test".to_string(),
                    kind: Kind::Local,
                    origin: None,
                    origin_key: "test:1".to_string(),
                    width: None,
                    height: None,
                    pinned: false,
                },
            );
        }
        write(root, &index).expect("the planted index");
    }

    /// The digests `index.json` holds, in order: "which entry goes" is asked of
    /// the index and of the directory, because a sweep that unlinks a file and
    /// forgets the entry -- or the other way round -- passes one of the two.
    fn entries(root: &Path) -> Vec<String> {
        let mut held: Vec<String> = load(root, unix_seconds())
            .expect("the index reads back")
            .entries
            .into_keys()
            .collect();
        held.sort();
        held
    }

    fn on_disk(root: &Path) -> u64 {
        survey(root).expect("the cache is measurable").files
    }

    /// **INV-CACHE-1 (bound), the count cap at its tightest.** 5.4 states the
    /// check at `cache.max_files` 1, so the sweep is driven there: three
    /// unprotected entries, and afterwards the unprotected set is at or under the
    /// cap, the survivor is the one with the newest `last_used` (5.5 step 5's
    /// order, not the file's own), and the directory and the index agree.
    ///
    /// A sweep that stopped one entry short (`files == 2`) or that evicted by name
    /// rather than by `last_used` (the survivor a different digest) fails here.
    #[test]
    fn inv_cache_1_the_count_cap_binds_at_1_and_the_newest_last_used_survives() {
        let root = scratch("inv-1-count");
        let oldest = plant(&root, &digest('1'), "jpg", 100);
        let middle = plant(&root, &digest('2'), "jpg", 100);
        let newest = plant(&root, &digest('3'), "jpg", 100);
        plant_index(
            &root,
            &[
                (&digest('1'), "2026-01-01T00:00:00Z"),
                (&digest('2'), "2026-01-02T00:00:00Z"),
                (&digest('3'), "2026-01-03T00:00:00Z"),
            ],
        );

        // Every file is past `cache.grace_seconds`: the grace window is 5.3's
        // protection and 5.4's step 4 has to be able to see it end.
        let now = unix_seconds() + 1_000;
        let swept = sweep(&root, &config(1, 1 << 30, 10), &Protected::default(), now)
            .expect("the sweep runs");

        assert_eq!(swept.removed, 2, "two entries over a cap of one");
        assert_eq!(swept.files, 1, "5.5 step 7's `files=` is measured after");
        assert!(!oldest.exists() && !middle.exists(), "the two oldest go");
        assert!(newest.exists(), "the newest `last_used` survives");
        assert_eq!(on_disk(&root), 1, "5.4's own count: `find sha256 -type f`");
        assert_eq!(entries(&root), vec![digest('3')]);
    }

    /// **INV-CACHE-1 (bound), the byte cap under `cache.max_files` N.** The
    /// default `max_files` is 500, so the count cap cannot bind and the byte cap
    /// is the one that has to: three 1000-byte files against 2500 bytes evict
    /// exactly one, the oldest `last_used`, and stop.
    ///
    /// A sweep that evicted to the byte cap greedily-or-not -- two files -- or
    /// that ignored the byte cap because the count cap held, fails here.
    #[test]
    fn inv_cache_1_the_byte_cap_binds_under_the_count_cap_n() {
        let root = scratch("inv-1-bytes");
        let oldest = plant(&root, &digest('4'), "jpg", 1_000);
        let middle = plant(&root, &digest('5'), "jpg", 1_000);
        let newest = plant(&root, &digest('6'), "jpg", 1_000);
        plant_index(
            &root,
            &[
                (&digest('4'), "2026-01-01T00:00:00Z"),
                (&digest('5'), "2026-01-02T00:00:00Z"),
                (&digest('6'), "2026-01-03T00:00:00Z"),
            ],
        );

        let now = unix_seconds() + 1_000;
        let swept = sweep(&root, &config(500, 2_500, 10), &Protected::default(), now)
            .expect("the sweep runs");

        assert_eq!(swept.removed, 1, "3000 bytes against 2500 frees one file");
        assert_eq!(swept.bytes, 2_000, "and the survivor set is measured");
        assert!(!oldest.exists());
        assert!(middle.exists() && newest.exists());
        assert_eq!(on_disk(&root), 2);
        assert_eq!(entries(&root), vec![digest('5'), digest('6')]);
    }

    /// **INV-CACHE-2 (display).** The anchor is the oldest entry in the cache, so
    /// 5.5 step 5 would evict it first if 5.3 did not protect it. After the sweep
    /// the anchor's file exists, byte for byte, at the path the anchor names, and
    /// it is the only file left against a count cap of 1.
    ///
    /// The mutation this catches is a sweep that orders by `last_used` alone: the
    /// anchor then goes first, which is the failure 5.3 exists to prevent.
    #[test]
    fn inv_cache_2_the_anchors_file_exists_and_is_not_evicted_at_cap_1() {
        let root = scratch("inv-2-anchor");
        let anchor = digest('a');
        let anchor_file = plant(&root, &anchor, "jpg", 700);
        let bytes = std::fs::read(&anchor_file).expect("the anchor's bytes");
        plant(&root, &digest('b'), "jpg", 100);
        plant(&root, &digest('c'), "jpg", 100);
        plant_index(
            &root,
            &[
                (&anchor, "2026-01-01T00:00:00Z"),
                (&digest('b'), "2026-01-02T00:00:00Z"),
                (&digest('c'), "2026-01-03T00:00:00Z"),
            ],
        );

        let protected = Protected {
            anchor_digest: Some(anchor.clone()),
            ..Protected::default()
        };
        let now = unix_seconds() + 1_000;
        let swept = sweep(&root, &config(1, 1 << 30, 10), &protected, now).expect("the sweep runs");

        assert_eq!(swept.removed, 2, "the two unprotected entries go");
        assert!(
            anchor_file.exists(),
            "the file the anchor names is still there"
        );
        assert_eq!(
            std::fs::read(&anchor_file).expect("the anchor's bytes"),
            bytes,
            "5.4: the anchor is not modified by whirl"
        );
        assert_eq!(on_disk(&root), 1);
        assert_eq!(entries(&root), vec![anchor]);
        let measured = survey(&root).expect("the cache is measurable");
        assert_eq!(
            over_cause(&measured, 1 << 30, 1, &protected, now, 10),
            None,
            "one file against a cap of one fits, so `cache_over_cap` is 0"
        );
    }

    /// **INV-CACHE-3 (pins).** Two pins, each protected by a different key
    /// because either can be the only one available: one by digest, through the
    /// index, and one by path only, which is a file the index does not know (a
    /// hand-run worker between rename and report) that `favorites.json` names.
    /// Both survive a sweep that evicts both unprotected entries at a count cap
    /// of 1, and the resulting overshoot is *reported* rather than hidden.
    ///
    /// The mutation this catches is a sweep that reclaims an unindexed file
    /// before asking whether it is pinned: that removes the pin's only copy, and
    /// 5.3 makes protection absolute.
    #[test]
    fn inv_cache_3_no_pinned_file_is_removed_at_cap_1() {
        let root = scratch("inv-3-pins");
        let pinned_digest = digest('d');
        let pin = plant(&root, &pinned_digest, "jpg", 400);
        // Unindexed and therefore `sha256/`'s second kind of orphan, protected by
        // the path `favorites.json` recorded for it.
        let unindexed = digest('e');
        let unindexed_pin = plant(&root, &unindexed, "jpg", 400);
        plant(&root, &digest('f'), "jpg", 100);
        plant(&root, &digest('0'), "jpg", 100);
        plant_index(
            &root,
            &[
                (&pinned_digest, "2026-01-02T00:00:00Z"),
                (&digest('f'), "2026-01-03T00:00:00Z"),
                (&digest('0'), "2026-01-04T00:00:00Z"),
            ],
        );

        let mut protected = Protected {
            pinned_digests: BTreeSet::from([pinned_digest.clone()]),
            ..Protected::default()
        };
        protected
            .pinned_paths
            .insert(unindexed_pin.to_string_lossy().to_string());
        let now = unix_seconds() + 1_000;
        let swept = sweep(&root, &config(1, 1 << 30, 10), &protected, now).expect("the sweep runs");

        assert_eq!(swept.removed, 2, "the two unprotected entries go");
        assert_eq!(swept.reclaimed, 0, "the unindexed pinned file is not one");
        assert!(pin.exists(), "the pinned digest's file survives");
        assert!(
            unindexed_pin.exists(),
            "5.3: protection by path is protection"
        );
        assert_eq!(on_disk(&root), 2, "two protected files against a cap of 1");
        assert_eq!(entries(&root), vec![pinned_digest]);
        let measured = survey(&root).expect("the cache is measurable");
        assert_eq!(
            over_cause(&measured, 1 << 30, 1, &protected, now, 10),
            Some(Cause::Pinned),
            "5.4: the protected set itself is the cause, and it is named"
        );
        assert_eq!(swept.hidden, 2, "2.9's `hidden`: kept only as pins");
    }

    /// 5.5 step 3's first direction, which is the other half of the same
    /// reconciliation: an index entry whose file is gone is not an entry the
    /// sweep evicts, it is one `status` reports (`dangling`) and the index stops
    /// claiming.
    #[test]
    fn an_index_entry_whose_file_is_missing_moves_to_dangling() {
        let root = scratch("dangling");
        let gone = digest('7');
        let present = digest('8');
        plant(&root, &present, "jpg", 100);
        plant_index(
            &root,
            &[
                (&gone, "2026-01-01T00:00:00Z"),
                (&present, "2026-01-02T00:00:00Z"),
            ],
        );

        let now = unix_seconds() + 1_000;
        sweep(&root, &config(500, 1 << 30, 10), &Protected::default(), now)
            .expect("the sweep runs");

        let index = load(&root, now).expect("the index reads back");
        assert_eq!(index.dangling, vec![gone]);
        assert_eq!(entries(&root), vec![present]);
        assert_eq!(on_disk(&root), 1);
    }

    /// 5.5 step 3's second direction, and both halves of 5.3's third rule: a
    /// `sha256/` file the index does not know is an orphan -- the case is a
    /// daemon killed between the rename and the report -- and it is reclaimed
    /// once it is past `cache.grace_seconds`, because until then it could be the
    /// in-flight file of a live writer.
    ///
    /// A sweep that reclaimed it inside the window, or that left it forever, fails
    /// one of the two assertions; both are the same code path.
    #[test]
    fn an_orphan_under_sha256_is_kept_inside_the_grace_and_reclaimed_after_it() {
        let root = scratch("orphan-grace");
        let orphan = digest('9');
        let path = plant(&root, &orphan, "jpg", 500);
        let now = unix_seconds();

        let inside = sweep(&root, &config(500, 1 << 30, 1), &Protected::default(), now)
            .expect("the sweep runs");
        assert_eq!(inside.reclaimed, 0);
        assert!(
            path.exists(),
            "5.3 protects a file created within `cache.grace_seconds`"
        );
        assert_eq!(inside.files, 1, "and it is still measured against the caps");

        let after = sweep(
            &root,
            &config(500, 1 << 30, 1),
            &Protected::default(),
            now + 2,
        )
        .expect("the sweep runs");
        assert_eq!(after.reclaimed, 1);
        assert!(!path.exists(), "past the window it is an orphan and goes");
        assert_eq!(after.freed_bytes, 500);
        assert_eq!(on_disk(&root), 0);
    }

    /// 5.5 step 4: a `tmp/*.part` is removed "whatever [it is], because a part
    /// file has no other owner", but only after `cache.orphan_grace_seconds`,
    /// which is the window a killed worker's part file is protected by.
    ///
    /// A sweep that cleaned `tmp/` on every run fails the first assertion, and
    /// one that never did fails the second; the two are the same branch.
    #[test]
    fn a_tmp_part_file_is_removed_only_after_the_orphan_grace_seconds() {
        let root = scratch("orphan-part");
        let parts = tmp_dir(&root);
        std::fs::create_dir_all(&parts).expect("tmp/");
        let part = parts.join("run-7-3f2a.part");
        std::fs::write(&part, vec![b'p'; 300]).expect("a part file");
        let now = unix_seconds();

        let inside = sweep(
            &root,
            &config(500, 1 << 30, 600),
            &Protected::default(),
            now,
        )
        .expect("the sweep runs");
        assert_eq!(inside.orphans, 0);
        assert!(part.exists(), "inside `cache.orphan_grace_seconds`");

        let after = sweep(
            &root,
            &config(500, 1 << 30, 600),
            &Protected::default(),
            now + 301,
        )
        .expect("the sweep runs");
        assert_eq!(after.orphans, 1);
        assert!(!part.exists());
        assert_eq!(after.freed_bytes, 300);
    }

    /// 5.3's escalation, which 5.5 restates step by step: while the pin set is
    /// unreadable nothing under `sha256/` is removed, and `tmp/` is still cleaned
    /// because a part file is never a pin.
    #[test]
    fn a_degraded_pin_set_protects_sha256_and_still_cleans_tmp() {
        let root = scratch("degraded");
        let images = [
            plant(&root, &digest('a'), "jpg", 100),
            plant(&root, &digest('b'), "jpg", 100),
        ];
        let parts = tmp_dir(&root);
        std::fs::create_dir_all(&parts).expect("tmp/");
        let part = parts.join("run-9-dead.part");
        std::fs::write(&part, b"half a download").expect("a part file");

        let protected = Protected {
            degraded: true,
            ..Protected::default()
        };
        let now = unix_seconds() + 1_000;
        let swept = sweep(&root, &config(1, 1 << 30, 10), &protected, now).expect("the sweep runs");

        assert_eq!(swept.removed, 0, "5.5 step 5 evicts nothing while degraded");
        assert_eq!(swept.reclaimed, 0, "and step 3 reclaims nothing either");
        assert!(images.iter().all(|path| path.exists()));
        assert_eq!(on_disk(&root), 2);
        assert_eq!(swept.orphans, 1, "5.5 step 4 still removes the part file");
        assert!(!part.exists());
        let measured = survey(&root).expect("the cache is measurable");
        assert_eq!(
            over_cause(&measured, 1 << 30, 1, &protected, now, 10),
            Some(Cause::Pinned),
            "and the overshoot is reported rather than hidden (6.4)"
        );
    }

    /// The half of the rule that gets broken: the sweep removes files under
    /// `sha256/` and `tmp/` of its own cache root and nothing else. A sibling
    /// file next to the cache root and a stray file inside it are not candidates
    /// for any of 5.5's three removal steps.
    #[test]
    fn the_sweep_touches_nothing_outside_sha256_and_tmp() {
        let root = scratch("outside");
        let evicted = plant(&root, &digest('1'), "jpg", 100);
        let kept = plant(&root, &digest('2'), "jpg", 100);
        plant_index(
            &root,
            &[
                (&digest('1'), "2026-01-01T00:00:00Z"),
                (&digest('2'), "2026-01-02T00:00:00Z"),
            ],
        );
        let stray = root.join("notes.txt");
        std::fs::write(&stray, "not a cache file").expect("a stray file");
        let sibling = std::env::temp_dir().join(format!(
            "whirl-cache-{}-outside-sibling.txt",
            std::process::id()
        ));
        std::fs::write(&sibling, "not in the cache at all").expect("a sibling file");

        let now = unix_seconds() + 1_000;
        let swept = sweep(&root, &config(1, 1 << 30, 10), &Protected::default(), now)
            .expect("the sweep runs");

        assert_eq!(swept.removed, 1);
        assert!(!evicted.exists(), "the over-cap entry goes");
        assert!(kept.exists());
        assert!(stray.exists(), "a file in the cache root but not an image");
        assert!(sibling.exists(), "a file outside the cache root");
        let _ = std::fs::remove_file(&sibling);
    }

    /// 5.5 step 6: the index is written even when nothing changed, "so that `seq`
    /// and `written_at` are honest about the last time the sweep ran", and 2.1's
    /// `root_id` is minted once for the cache directory -- on the way to a write,
    /// so two readers of the same cache cannot disagree about its identity -- and
    /// then read back rather than re-minted.
    #[test]
    fn the_sweep_writes_the_index_even_when_nothing_changed() {
        let root = scratch("empty");
        assert_eq!(
            load(&root, unix_seconds()).expect("an empty index").root_id,
            "",
            "a cache nothing has written has no identity yet (2.1)"
        );

        let now = unix_seconds();
        sweep(
            &root,
            &config(500, 1 << 30, 600),
            &Protected::default(),
            now,
        )
        .expect("the first sweep");
        let first = load(&root, now).expect("the index reads back");
        assert_eq!(first.seq, 1, "step 6 bumps `seq` on an empty cache");
        assert_eq!(
            first.root_id.len(),
            36,
            "the minted id is an RFC 4122 uuid: {}",
            first.root_id
        );

        sweep(
            &root,
            &config(500, 1 << 30, 600),
            &Protected::default(),
            now,
        )
        .expect("the second sweep");
        let second = load(&root, now).expect("the index reads back");
        assert_eq!(second.seq, 2, "and every sweep after it");
        assert_eq!(
            second.root_id, first.root_id,
            "the identity is minted once per cache, not once per write"
        );
        assert!(
            root.join(INDEX_FILE).is_file(),
            "index.json is in the cache root (2.1)"
        );
    }
}
