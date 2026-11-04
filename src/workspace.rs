//! The per-instance scratch workspace.
//!
//! Every build, check, clippy, flash, size and rust-analyzer session runs
//! against a throw-away COPY of the project in a per-user cache directory (see
//! [`base`]) — never the user's own folder. That directory used to be one
//! hardcoded path, which made a second IDE window a data hazard rather than a
//! second window: both processes wrote their whole project into it on every
//! save, so `main.rs`, `Cargo.toml`, `memory.x` and the target triple in
//! `.cargo/config.toml` were whatever the last instance saved. On top of that,
//! `write_project`'s foreign-crate prune deleted the other project's libraries,
//! the notify watcher pulled the other project's files into this one's tree
//! (and from there into the real project folder on the next save), and the
//! stale-RA sweep — keyed by this very directory — shot the other instance's
//! analyzer.
//!
//! So each process now claims its own SLOT: `embedded_ide_0_check`,
//! `embedded_ide_0_check_2`, `embedded_ide_0_check_3`, … The first window still
//! gets the unsuffixed name, so the usual single-window case reads the same as
//! it always did.
//!
//! The slot NAMES survived the move out of `%TEMP%`; the paths did not. An
//! install upgrading across that move builds cold once, because its warm
//! `target/` is at the old location — which the sweep then reclaims rather than
//! leaving gigabytes stranded (see [`sweep_stale_slots`]).
//!
//! Claiming is an OS-level exclusive lock on a file inside the slot, held open
//! for the life of the process. That is deliberate: the kernel drops the handle
//! when the process dies *however* it dies, so a crash can never leave a slot
//! permanently reserved — no pid files, no liveness probes, no stale-lock
//! recovery path to get wrong.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// How many slots to try before giving up and going pid-unique. Well past any
/// plausible number of IDE windows; the cap only exists so a pathological
/// environment can't spin.
const MAX_SLOTS: u32 = 16;

/// The directory the slots live in.
///
/// Deliberately NOT the system temp directory, which is where they used to be.
/// `%TEMP%` is a place the OS is *invited* to clean: Windows Storage Sense's
/// "delete temporary files that my apps aren't using" removes files out of it
/// while leaving the directories standing, and a cargo `target/` cannot survive
/// that. Cargo fingerprints a build script by its INPUTS and never by what it
/// wrote, so ONE deleted file under `target/…/build/<pkg>-<hash>/out/` leaves
/// the script looking fresh for good: it is never re-run, and every Build and
/// Clippy fails on a missing `include!`. That is not a theory — it happened
/// here, and [`crate::failure_hint::stale_out_dir`] exists to explain the
/// wreckage to the user. This function is the other half of that fix: stop
/// putting a build cache somewhere a cleaner is entitled to empty.
///
/// A build cache must also not ROAM. `%APPDATA%` follows the user to every
/// machine on a domain profile, and a 16 GB `target/` is not a thing to
/// synchronise — hence LOCAL app data on Windows, and the CACHE directory (not
/// the config one [`crate::panels::mcu_module::registry::user_config_dir`]
/// uses) elsewhere.
fn base() -> PathBuf {
    let cache = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Caches"))
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    };
    // No resolvable cache home is the one case where temp is still the answer:
    // it is a worse place, but this module's contract is that `dir()` always
    // answers, and refusing to build at all would be worse still.
    cache
        .map(|b| b.join(crate::names::LEGACY_DATA_DIR))
        .unwrap_or_else(legacy_base)
}

/// The system temp directory — where slots lived before [`base`] moved them.
///
/// Named once, here, so the sweep can still find and reclaim what an install
/// upgrading from an older build left behind: those directories hold gigabytes
/// and nothing else will ever look for them again.
fn legacy_base() -> PathBuf {
    std::env::temp_dir()
}

/// The claimed slot's directory + its 1-based number, resolved once by [`init`].
static WORKSPACE: OnceLock<(PathBuf, u32)> = OnceLock::new();

/// The open lock file. Never read — kept alive so the OS keeps the slot
/// reserved for as long as this process runs.
static LOCK: OnceLock<std::fs::File> = OnceLock::new();

/// The slot a scripted run claims (`app::automation`, debug builds with
/// `EIDE_AUTOMATION` set): a scratch directory, an eframe storage name and so
/// a remembered project of its own. A smoke run that took the user's slot 1,
/// free whenever their window is closed, would leave its test project as the
/// one their next start opens. Above [`MAX_SLOTS`], so no
/// window ever claims it otherwise; reused run after run, so its `target/`
/// stays warm.
pub const AUTOMATION_SLOT: u32 = 99;

/// The folder `EIDE_AUTOMATION` names, when this run is scripted: debug
/// builds only, and only an absolute path - an empty or relative one would
/// land wherever the IDE was started.
pub fn automation_dir() -> Option<PathBuf> {
    if !cfg!(debug_assertions) {
        return None;
    }
    automation_dir_from(std::env::var_os("EIDE_AUTOMATION"))
}

fn automation_dir_from(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    Some(PathBuf::from(value?)).filter(|d| d.is_absolute())
}

/// Is this a scripted run, in [`AUTOMATION_SLOT`] or a pid slot of its own?
/// It then leaves the user's shared state alone: the recent-projects list
/// is not written, and its crash log and project locks go into the
/// automation folder (`build::crash_log_path`, [`claim_project`]).
pub fn scripted() -> bool {
    automation_dir().is_some()
}

/// Claim a workspace slot. Call ONCE from `main`, before anything spawns a
/// thread or touches the workspace — every later reader gets the same answer.
pub fn init() {
    let base = base();
    // A scripted run never takes a user's slot: its own, or a pid one.
    let slots = if scripted() {
        AUTOMATION_SLOT..=AUTOMATION_SLOT
    } else {
        1..=MAX_SLOTS
    };
    for slot in slots {
        let dir = base.join(slot_name(slot));
        if std::fs::create_dir_all(&dir).is_err() {
            continue;
        }
        if let Some(file) = try_lock(&dir.join(".instance.lock")) {
            let _ = LOCK.set(file);
            let _ = WORKSPACE.set((dir, slot));
            return;
        }
    }
    // Every slot is taken (or the cache dir is unwritable). A pid-unique
    // directory is worse — a cold `target/` every launch — but it is still
    // correct, and correctness is the point of this module.
    let dir = base.join(format!(
        "{}_p{}",
        crate::names::LEGACY_SLOT_PREFIX,
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let _ = WORKSPACE.set((dir, 0));
}

/// How long an unclaimed slot may sit before its build cache is thrown away.
///
/// Not zero, and not "every slot but ours". A free slot is REUSABLE - the next
/// window takes it and inherits a warm `target/` - so deleting one the moment
/// nobody holds it would make a second window cold every single time. Two weeks
/// is long past "I closed that window an hour ago" and well short of forever.
const STALE_SLOT_DAYS: u64 = 14;

/// Delete workspace slots nobody has used in a fortnight.
///
/// Slots are reused, so their COUNT is bounded by the most windows ever open at
/// once - but a slot used once keeps its `target/` for good. Six of them had
/// accumulated on the author's machine holding 8.3 GB, on a disk that had
/// reached zero bytes free; nothing in the IDE had ever removed one.
///
/// Runs on its own thread: the scan is cheap but the delete is gigabytes, and
/// startup must not wait for a disk.
///
/// Safety comes from the same lock that claims a slot. A directory is only
/// removed when its lock can be taken - which means no live process holds it -
/// AND it is older than [`STALE_SLOT_DAYS`]. Our own slot is skipped outright:
/// we hold its lock, so it could never pass the first test anyway, and relying
/// on that would be a subtle way to depend on lock semantics from two places.
pub fn sweep_stale_slots() {
    let Some((ours, _)) = WORKSPACE.get().cloned() else {
        return;
    };
    std::thread::spawn(move || {
        let cutoff = std::time::Duration::from_secs(STALE_SLOT_DAYS * 24 * 60 * 60);
        sweep_base(&base(), &ours, cutoff);

        // Slots at the OLD location are not stale, they are STRANDED: nothing
        // will ever claim one again, so the reasoning behind
        // [`STALE_SLOT_DAYS`] — keep a free slot warm, the next window reuses
        // it — does not apply to them at all. The only question left is whether
        // a window from an older build still holds the lock, and `sweepable`
        // answers that. Making these wait a fortnight would leave gigabytes
        // nobody can use on the disk this sweep exists to protect.
        let legacy = legacy_base();
        if legacy != base() {
            sweep_base(&legacy, &ours, std::time::Duration::ZERO);
        }
    });
}

/// Sweep one directory of slots. Split from [`sweep_stale_slots`] so the new
/// and legacy locations can share it on different cutoffs.
fn sweep_base(base: &Path, ours: &Path, cutoff: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if sweepable(&dir, ours, cutoff) {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// May `dir` be deleted? Every guard the sweep has, in one place.
///
/// Split out of the loop so it can be TESTED. A sweep whose decision lives
/// inside a spawned thread over the real temp directory is a sweep nobody can
/// check, and this one deletes gigabytes.
fn sweepable(dir: &Path, ours: &Path, cutoff: std::time::Duration) -> bool {
    if dir == ours || !dir.is_dir() {
        return false;
    }
    let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if !name.starts_with(crate::names::LEGACY_SLOT_PREFIX) {
        return false;
    }
    // Age from the directory itself: a `target/` written yesterday updates it,
    // so "old" really does mean "nothing has been built here".
    let stale = std::fs::metadata(dir)
        .and_then(|m| m.modified())
        .and_then(|t| t.elapsed().map_err(std::io::Error::other))
        .is_ok_and(|age| age > cutoff);
    if !stale {
        return false;
    }
    // The pid-fallback directories carry no lock file, so the age gate is all
    // they get - which is right: nothing ever reuses one, and a fortnight-old
    // pid is not running.
    let lock_path = dir.join(".instance.lock");
    if lock_path.exists() {
        match try_lock(&lock_path) {
            // Held by a live window that simply has not built lately.
            None => return false,
            // Dropped at once: holding it would reserve a slot we are about to
            // delete.
            Some(f) => drop(f),
        }
    }
    true
}

/// Directory name for `slot`. Slot 1 is the historical path — unsuffixed, so an
/// existing install keeps its build cache.
fn slot_name(slot: u32) -> String {
    if slot <= 1 {
        crate::names::LEGACY_SLOT_PREFIX.to_owned()
    } else {
        format!("{}_{slot}", crate::names::LEGACY_SLOT_PREFIX)
    }
}

/// Absolute path of THIS instance's scratch workspace.
///
/// Falls back to claiming a slot on the spot if [`init`] never ran (a test
/// binary, say) — the return value must be stable for the whole process, so it
/// can never be left undefined.
pub fn dir() -> PathBuf {
    if WORKSPACE.get().is_none() {
        init();
    }
    WORKSPACE
        .get()
        .map(|(d, _)| d.clone())
        .unwrap_or_else(|| base().join(slot_name(1)))
}

/// This instance's slot number (1 = the first/only window, 0 = pid fallback).
pub fn slot() -> u32 {
    if WORKSPACE.get().is_none() {
        init();
    }
    WORKSPACE.get().map(|(_, s)| *s).unwrap_or(1)
}

/// Suffix that makes a per-instance name out of a shared one — empty for the
/// first instance, so its files keep the names they have always had.
///
/// For everything OUTSIDE the workspace directory that would otherwise be
/// shared between windows: the rust-analyzer log, the serial-bridge PTY links,
/// the eframe storage name.
pub fn suffix() -> String {
    match slot() {
        0 => format!("_p{}", std::process::id()),
        1 => String::new(),
        s => format!("_{s}"),
    }
}

// ── Project-folder lock ──────────────────────────────────────────────────────
// Isolating the scratch workspace stops two windows from corrupting each
// other's GENERATED project. It does nothing about the remaining hazard: the
// same project folder opened in both, where each `write_project` targets the
// user's real files and the last save silently wins.
//
// The claim uses the same OS-lock trick as the slots, but the lock file lives
// in the app's config dir rather than in the project. A file inside the project
// would show up in the tree, in `git status`, and in the stale-file prune — a
// permanent footprint for a transient fact.

/// A held claim on a project folder. Dropping it releases the folder.
pub struct ProjectLock {
    /// Kept open for the life of the claim; the OS releases it on exit.
    _file: std::fs::File,
}

/// The result of trying to claim a project folder.
pub enum ProjectClaim {
    /// This process now owns the folder.
    Acquired(ProjectLock),
    /// Another live IDE window owns it.
    Busy,
    /// Locking isn't possible here (no config dir, unwritable). Treated as
    /// "fine" by callers: a missing lock must never produce a false warning.
    Unavailable,
}

/// Try to claim `project_dir` for this instance.
///
/// The caller must drop its previous claim FIRST — reopening the same project
/// would otherwise report itself as busy.
pub fn claim_project(project_dir: &Path) -> ProjectClaim {
    let Some(path) = project_lock_path(project_dir) else {
        return ProjectClaim::Unavailable;
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return ProjectClaim::Unavailable;
        }
    }
    match try_lock(&path) {
        Some(file) => ProjectClaim::Acquired(ProjectLock { _file: file }),
        None => ProjectClaim::Busy,
    }
}

/// `<config>/locks/<leaf>-<hash>.lock` for a project directory.
///
/// The hash is what identifies the folder (the leaf name is only there to make
/// the directory readable when something goes wrong). Case-insensitive on
/// Windows, where `C:\Proj` and `c:\proj` are the same folder and must not be
/// claimable twice.
fn project_lock_path(project_dir: &Path) -> Option<PathBuf> {
    use std::hash::{Hash as _, Hasher as _};
    // Canonicalize so `.`, `..` and a trailing separator can't disguise the
    // same folder as a different one. A path that can't be canonicalized (it
    // was just deleted) falls back to its literal form.
    let canonical =
        std::fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    let mut key = canonical.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        key = key.to_lowercase();
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    let leaf: String = canonical
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_owned())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(32)
        .collect();
    // A scripted run opens throwaway projects: their locks go with its
    // folder, not one by one into the user's config dir for good.
    let root = match automation_dir() {
        Some(dir) => dir,
        None => crate::panels::mcu_module::registry::user_config_dir()?,
    };
    Some(
        root.join("locks")
            .join(format!("{leaf}-{:016x}.lock", hasher.finish())),
    )
}

/// Open `path` with an exclusive, non-blocking lock. `None` = another live
/// process holds it.
#[cfg(windows)]
fn try_lock(path: &Path) -> Option<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    // share_mode(0): no other process may open this file at all, which is
    // exactly the claim we want. Windows releases it when the process exits,
    // crash included.
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .share_mode(0)
        .open(path)
        .ok()
}

#[cfg(unix)]
fn try_lock(path: &Path) -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(path)
        .ok()?;
    // LOCK_NB so a taken slot fails instantly instead of blocking startup.
    // The lock is released with the fd when the process exits.
    let ok = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    ok.then_some(file)
}

#[cfg(not(any(windows, unix)))]
fn try_lock(_path: &Path) -> Option<std::fs::File> {
    None // unknown platform: fall through to the pid-unique directory
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_one_keeps_the_historical_path() {
        assert_eq!(slot_name(1), "embedded_ide_0_check");
        assert_eq!(slot_name(2), "embedded_ide_0_check_2");
    }

    /// A scripted run's slot is no window's: above every slot a window takes,
    /// and swept by the same rule when left unused. Its folder must be named
    /// absolutely, or there is no scripted run at all.
    #[test]
    fn a_scripted_run_has_a_slot_of_its_own() {
        const { assert!(AUTOMATION_SLOT > MAX_SLOTS) };
        let name = slot_name(AUTOMATION_SLOT);
        assert!(name.starts_with(crate::names::LEGACY_SLOT_PREFIX), "{name}");
        assert!((1..=MAX_SLOTS).all(|s| slot_name(s) != name));
        let abs = std::env::temp_dir().join("eide-auto");
        assert_eq!(automation_dir_from(Some(abs.clone().into())), Some(abs));
        for refused in [None, Some("".into()), Some("auto".into())] {
            assert_eq!(automation_dir_from(refused), None);
        }
    }

    /// The whole point of [`base`]: a cargo `target/` must not sit where the OS
    /// is entitled to delete files out from under it.
    #[test]
    fn the_workspace_lives_outside_the_system_temp_dir() {
        let b = base();
        assert!(
            b.ends_with("embedded_ide_0"),
            "the base must be app-specific or a sweep would scan a shared dir: {}",
            b.display()
        );
        // A machine with no resolvable cache home legitimately falls back to
        // temp, so only assert the move where the env can actually answer.
        let home_var = if cfg!(windows) {
            "LOCALAPPDATA"
        } else {
            "HOME"
        };
        if std::env::var_os(home_var).is_some() {
            assert!(
                !b.starts_with(legacy_base()),
                "the scratch workspace is back under the OS temp dir, which cleaners \
                 empty while leaving the directories — the exact fault that made every \
                 build fail on a missing OUT_DIR file: {}",
                b.display()
            );
        }
    }

    /// `temp_dir()` inside THIS file, outside `legacy_base`, is the regression
    /// that would undo the move — and the sibling guard
    /// (`nothing_rebuilds_the_workspace_path_by_hand`) cannot catch it, because
    /// it skips `workspace.rs` by name. This file is where the bug would live.
    ///
    /// The base is read in three places — [`init`], the sweep and [`dir`]'s
    /// fallback. One of them left behind is not a compile error: it is a slot
    /// nobody ever sweeps, which is how six of them once reached 8.3 GB on a
    /// disk at zero bytes free.
    #[test]
    fn only_the_legacy_hook_may_name_the_temp_dir() {
        let src =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/workspace.rs"))
                .expect("read this module's own source");
        // Tests are allowed it — their scratch dirs belong in temp.
        let head = src.split("#[cfg(test)]").next().unwrap_or_default();
        let hits: Vec<&str> = head
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .filter(|l| l.contains("temp_dir()"))
            .map(str::trim)
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "the system temp dir may be named exactly ONCE, in `legacy_base`; \
             call `base()` instead:\n{}",
            hits.join("\n")
        );
        let (_, after) = head
            .split_once("fn legacy_base")
            .expect("`legacy_base` must still exist — the sweep needs it to reclaim old slots");
        assert!(
            after.lines().take(4).any(|l| l.contains("temp_dir()")),
            "the one remaining `temp_dir()` is no longer `legacy_base`'s"
        );
    }

    /// A held lock must refuse the next claim — that is the whole mechanism.
    /// Same-process locking counts: Windows share_mode and unix `flock` are
    /// both per-open-file-description, not per-process-and-forgiving.
    #[test]
    fn a_locked_slot_cannot_be_claimed_twice() {
        let dir = std::env::temp_dir().join(format!("eide_slot_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".instance.lock");
        let held = try_lock(&path).expect("first claim must succeed");
        assert!(
            try_lock(&path).is_none(),
            "a slot already claimed must not be handed out again"
        );
        drop(held);
        assert!(
            try_lock(&path).is_some(),
            "releasing the lock must free the slot"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The isolation only holds if EVERY reader goes through [`dir`]. There
    /// were 22 hand-rolled `temp_dir().join("embedded_ide_0_check")` sites
    /// before this module existed, and one re-introduced anywhere silently
    /// pins that feature back onto the first instance's workspace — a bug that
    /// shows up as cross-window corruption, not as a compile error. So the
    /// literal is banned outside this file, in the spirit of `glyph_guard`.
    #[test]
    fn nothing_rebuilds_the_workspace_path_by_hand() {
        fn scan(dir: &Path, out: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    scan(&p, out);
                    continue;
                }
                if p.extension().and_then(|x| x.to_str()) != Some("rs")
                    // This module owns the name (and its own fallback).
                    || p.file_name().and_then(|x| x.to_str()) == Some("workspace.rs")
                {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&p) else {
                    continue;
                };
                for (i, line) in text.lines().enumerate() {
                    let t = line.trim_start();
                    // Comments, doc-comments and test fixtures may SAY the
                    // name; only code that joins it onto the temp dir is a bug.
                    if t.starts_with("//") {
                        continue;
                    }
                    if line.contains("temp_dir()") && line.contains("embedded_ide_0_check") {
                        out.push(format!("{}:{}  {}", p.display(), i + 1, t));
                    }
                }
            }
        }
        let mut bad = Vec::new();
        scan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut bad);
        assert!(
            bad.is_empty(),
            "the scratch workspace is PER INSTANCE - call \
             crate::workspace::dir() instead of rebuilding the path, or a \
             second IDE window will write into this one's workspace:\n{}",
            bad.join("\n")
        );
    }

    /// The same folder must map to ONE lock file however it is spelled —
    /// otherwise two windows both "claim" it and the warning never fires.
    #[test]
    fn project_lock_path_identifies_the_folder_not_the_spelling() {
        let base = std::env::temp_dir().join(format!("eide_lockpath_{}", std::process::id()));
        let proj = base.join("Proj");
        std::fs::create_dir_all(&proj).unwrap();

        let direct = project_lock_path(&proj);
        let indirect = project_lock_path(&base.join("Proj").join("."));
        assert!(
            direct.is_some(),
            "a config dir resolves in this environment"
        );
        assert_eq!(direct, indirect, "`.` must not look like another project");

        // A different folder must NOT collide.
        let other = base.join("Other");
        std::fs::create_dir_all(&other).unwrap();
        assert_ne!(direct, project_lock_path(&other));
    }

    /// End to end: the second claim on a folder is refused, and releasing the
    /// first hands it back — the behaviour the warning banner reads.
    #[test]
    fn a_project_can_only_be_claimed_by_one_instance() {
        let dir = std::env::temp_dir().join(format!("eide_claim_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let first = claim_project(&dir);
        if matches!(first, ProjectClaim::Unavailable) {
            return; // no config dir here — locking is opt-out, not a failure
        }
        assert!(matches!(first, ProjectClaim::Acquired(_)));
        assert!(
            matches!(claim_project(&dir), ProjectClaim::Busy),
            "a project already open elsewhere must report Busy"
        );
        drop(first);
        assert!(
            matches!(claim_project(&dir), ProjectClaim::Acquired(_)),
            "closing the other window must free the project"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The names the OTHER shared files are built from.
    #[test]
    fn suffix_is_empty_only_for_the_first_instance() {
        // `suffix` reads the process-wide slot, so assert on the mapping it
        // uses rather than on whichever slot this test binary happened to get.
        let name = |slot: u32| match slot {
            0 => "_p<pid>".to_owned(),
            1 => String::new(),
            s => format!("_{s}"),
        };
        assert_eq!(name(1), "");
        assert_eq!(name(3), "_3");
    }
}

#[cfg(test)]
mod stale_slot_sweep {
    use super::{sweep_base, sweepable, try_lock};
    use std::time::Duration;

    /// Everything is old (cutoff 0) / nothing is (cutoff a century).
    const OLD: Duration = Duration::from_secs(0);
    const NEVER: Duration = Duration::from_secs(100 * 365 * 24 * 60 * 60);

    /// A scratch directory that removes itself when the test ends.
    ///
    /// Not a bare path plus a `remove_dir_all` at the bottom of each test: two
    /// of the five forgot it, and left directories in the very temp folder this
    /// module exists to stop filling up.
    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl std::ops::Deref for Scratch {
        type Target = std::path::Path;
        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }

    fn scratch(name: &str) -> Scratch {
        let d = std::env::temp_dir().join(format!("eide_sweep_test_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("scratch");
        Scratch(d)
    }

    /// A slot nobody holds, old enough, goes.
    #[test]
    fn an_abandoned_slot_is_swept() {
        let base = scratch("abandoned");
        let slot = base.join("embedded_ide_0_check_9");
        std::fs::create_dir_all(&slot).unwrap();
        std::fs::write(slot.join(".instance.lock"), b"").unwrap();
        assert!(sweepable(&slot, &base, OLD));
    }

    /// A LIVE window keeps its slot, however long since it last built.
    ///
    /// This is the guard that matters: the lock is the only thing standing
    /// between the sweep and another window's workspace.
    #[test]
    fn a_held_slot_is_never_swept() {
        let base = scratch("held");
        let slot = base.join("embedded_ide_0_check_9");
        std::fs::create_dir_all(&slot).unwrap();
        let held = try_lock(&slot.join(".instance.lock")).expect("take the lock");
        assert!(!sweepable(&slot, &base, OLD), "a locked slot is in use");
        drop(held);
        // ...and once released, the same directory is fair game.
        assert!(sweepable(&slot, &base, OLD));
    }

    /// A slot used recently keeps its warm `target/`.
    ///
    /// Slots are REUSED - deleting a free one immediately would hand the next
    /// window a cold build every time.
    #[test]
    fn a_recent_slot_is_kept() {
        let base = scratch("recent");
        let slot = base.join("embedded_ide_0_check_2");
        std::fs::create_dir_all(&slot).unwrap();
        assert!(
            !sweepable(&slot, &base, NEVER),
            "just created, so not stale"
        );
    }

    /// A stranded slot at the OLD location goes at once, brand new or not.
    ///
    /// The fortnight exists because a free slot is REUSABLE. One left in
    /// `%TEMP%` is not: `base()` never looks there again, so waiting only keeps
    /// gigabytes nobody can reach. The zero cutoff is what expresses that, and
    /// the lock is still the thing that protects a live older window.
    #[test]
    fn a_stranded_legacy_slot_goes_without_waiting() {
        let base = scratch("stranded");
        let slot = base.join("embedded_ide_0_check");
        std::fs::create_dir_all(&slot).unwrap();
        let zero = OLD;
        assert!(
            sweepable(&slot, &base, zero),
            "a just-created legacy slot is still unreachable, so it must go"
        );
        // ...unless an older window is still in it.
        let held = try_lock(&slot.join(".instance.lock")).expect("take the lock");
        assert!(
            !sweepable(&slot, &base, zero),
            "a legacy slot a live window still holds must survive the migration"
        );
        drop(held);
    }

    /// `sweep_base` must not walk out of the directory it was given, and must
    /// not touch anything that is not a slot.
    #[test]
    fn sweeping_a_base_spares_everything_that_is_not_a_slot() {
        let base = scratch("selective");
        let slot = base.join("embedded_ide_0_check_7");
        let bystander = base.join("someone_elses_cache");
        let ours = base.join("embedded_ide_0_check");
        for d in [&slot, &bystander, &ours] {
            std::fs::create_dir_all(d).unwrap();
        }
        sweep_base(&base, &ours, OLD);
        assert!(!slot.exists(), "an unheld slot should have been swept");
        assert!(
            bystander.exists(),
            "a directory that is not a slot is not ours"
        );
        assert!(ours.exists(), "our own slot must never be swept");
    }

    /// Our own slot, and anything that is not a slot at all, are untouchable.
    #[test]
    fn our_own_slot_and_strangers_are_untouchable() {
        let base = scratch("ours");
        let ours = base.join("embedded_ide_0_check");
        std::fs::create_dir_all(&ours).unwrap();
        assert!(!sweepable(&ours, &ours, OLD), "never our own workspace");

        let stranger = base.join("someone_elses_cache");
        std::fs::create_dir_all(&stranger).unwrap();
        assert!(!sweepable(&stranger, &base, OLD), "name gate");

        let file = base.join("embedded_ide_0_check_not_a_dir");
        std::fs::write(&file, b"x").unwrap();
        assert!(!sweepable(&file, &base, OLD), "files are not slots");
    }

    /// A pid-fallback directory has no lock file; the age gate alone decides.
    #[test]
    fn a_pid_fallback_dir_is_swept_on_age_alone() {
        let base = scratch("pid");
        let slot = base.join("embedded_ide_0_check_p999999");
        std::fs::create_dir_all(&slot).unwrap();
        assert!(!slot.join(".instance.lock").exists());
        assert!(sweepable(&slot, &base, OLD));
        assert!(!sweepable(&slot, &base, NEVER), "and age still gates it");
    }
}
