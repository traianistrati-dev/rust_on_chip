
// Everything below is editable — your changes are preserved on regeneration.
//
// Restart on panic. main.rs's `#[panic_handler]` calls `restart`: the panic's
// place and message go into a record in RAM that a software reset keeps, the
// chip waits, then restarts. The next boot finds the record in `start` and
// hands it to `report` (in the block above: the serial port, MQTT) and to
// `last()`, for your own code.
//
// The wait doubles with every panic in a row - FIRST_WAIT_MS, twice that, ...
// up to MAX_WAIT_MS - so a panic on every boot does not hammer the network. A
// boot that ran SETTLE_MS before panicking starts the count again, and so does
// a restart that was not a panic: power, the reset button, and - where the
// chip can tell (`restart_kept` above) or the firmware's code moved - a new
// firmware.
//
// It covers panics only: a hard fault or a hang needs the watchdog
// (Configuration tab).

use core::fmt::Write as _;
use core::sync::atomic::{AtomicBool, Ordering};

/// The wait before the first restart. It doubles with each panic in a row.
pub const FIRST_WAIT_MS: u32 = 1_000;
/// The longest wait between restarts.
pub const MAX_WAIT_MS: u32 = 60_000;
/// A boot that ran this long before its panic starts the count again.
pub const SETTLE_MS: u64 = 10 * 60 * 1_000;

/// The record, in 32-bit words: its mark, the count, "not reported yet", the
/// message's length, the message, a checksum.
const RECORD_WORDS: usize = 44;
const HEAD_WORDS: usize = 4;
const MESSAGE_BYTES: usize = (RECORD_WORDS - HEAD_WORDS - 1) * 4;
const MARK: u32 = 0x5043_0001;

/// The panic that restarted this boot.
#[derive(Clone, Copy)]
pub struct Report {
    /// Panics in a row, this one included.
    pub count: u32,
    len: usize,
    bytes: [u8; MESSAGE_BYTES],
}

impl Report {
    /// Where and what: `crate/src/file.rs:line: message`, cut to the record.
    pub fn text(&self) -> &str {
        let bytes = self.bytes.get(..self.len).unwrap_or(&[]);
        match core::str::from_utf8(bytes) {
            Ok(text) => text,
            Err(e) => core::str::from_utf8(bytes.get(..e.valid_up_to()).unwrap_or(&[])).unwrap_or(""),
        }
    }
}

impl core::fmt::Display for Report {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.count > 1 {
            write!(f, "restarted after a panic ({} in a row): {}", self.count, self.text())
        } else {
            write!(f, "restarted after a panic: {}", self.text())
        }
    }
}

/// Set once `restart` runs: a panic inside it does not start over.
static HANDLING: AtomicBool = AtomicBool::new(false);
/// Set by `start` when a panic restarted this boot.
static REPORTED: AtomicBool = AtomicBool::new(false);

/// The panic handler's work: record, wait, restart.
pub fn restart(info: &core::panic::PanicInfo) -> ! {
    restart_with(info.location().map(|at| (at.file(), at.line())), &info.message())
}

/// The same for a reason of your own - from a HardFault handler, say:
/// `restart_with(None, &"HardFault")`. Never returns.
pub fn restart_with(at: Option<(&str, u32)>, message: &dyn core::fmt::Display) -> ! {
    interrupts_off();
    if HANDLING.load(Ordering::Relaxed) {
        // A panic while handling one: what is recorded stands.
        wait_ms(wait_for(recorded_count().unwrap_or(1)));
        reset();
    }
    HANDLING.store(true, Ordering::Relaxed);
    let count = if uptime_ms() >= SETTLE_MS {
        1
    } else {
        recorded_count().unwrap_or(0).saturating_add(1)
    };
    // The place first, without core::fmt - all a panic on a spent stack
    // may get to write.
    let mut text = Text::new();
    if let Some((file, line)) = at {
        text.push_path(short_path(file));
        text.push_str(":");
        text.push_u32(line);
        text.push_str(": ");
    }
    store(count, &text);
    // Then the message.
    let _ = write!(text, "{message}");
    store(count, &text);
    let wait = wait_for(count);
    announce(text.as_str(), wait);
    wait_ms(wait);
    reset()
}

/// At boot, right after the HAL (main.rs calls it): a panic restarted the
/// chip - report it; anything else - forget the count.
pub fn start(spawner: embassy_executor::Spawner) {
    match load() {
        Some((report, true)) if restart_kept() => {
            // Reported once; the count stays for the next panic's wait.
            set_word(2, 0);
            seal();
            REPORTED.store(true, Ordering::Relaxed);
            report_at_boot(spawner, report);
        }
        _ => set_word(0, 0),
    }
}

/// The panic that restarted this boot, if one did.
pub fn last() -> Option<Report> {
    if !REPORTED.load(Ordering::Relaxed) {
        return None;
    }
    load().map(|(report, _)| report)
}

/// FIRST_WAIT_MS doubled per panic in a row, at most MAX_WAIT_MS.
fn wait_for(count: u32) -> u32 {
    let doublings = count.saturating_sub(1).min(16);
    FIRST_WAIT_MS.saturating_mul(1 << doublings).min(MAX_WAIT_MS)
}

/// The record's mark for THIS firmware: a record a build whose code moved
/// left (a fix flashed over a board that kept panicking) does not read as
/// this one's.
fn mark() -> u32 {
    let image = (restart as *const () as usize as u32) ^ (start as *const () as usize as u32).rotate_left(16);
    MARK ^ image ^ RECORD_WORDS as u32
}

fn word(i: usize) -> u32 {
    // SAFETY: `i` < RECORD_WORDS, in the record's own memory.
    unsafe { record().cast::<u32>().add(i).read_volatile() }
}

fn set_word(i: usize, value: u32) {
    // SAFETY: as in `word`.
    unsafe { record().cast::<u32>().add(i).write_volatile(value) }
}

/// FNV-1a over every word but the last, which holds it.
fn checksum() -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for i in 0..RECORD_WORDS - 1 {
        hash ^= word(i);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn seal() {
    set_word(RECORD_WORDS - 1, checksum());
}

fn valid() -> bool {
    word(0) == mark() && word(RECORD_WORDS - 1) == checksum()
}

fn recorded_count() -> Option<u32> {
    valid().then(|| word(1))
}

fn store(count: u32, text: &Text) {
    set_word(0, mark());
    set_word(1, count);
    set_word(2, 1);
    set_word(3, text.len as u32);
    for w in 0..MESSAGE_BYTES / 4 {
        let mut bytes = [0u8; 4];
        for (k, b) in bytes.iter_mut().enumerate() {
            *b = text.bytes.get(w * 4 + k).copied().unwrap_or(0);
        }
        set_word(HEAD_WORDS + w, u32::from_le_bytes(bytes));
    }
    seal();
}

/// The record, and whether it still waits to be reported.
fn load() -> Option<(Report, bool)> {
    if !valid() {
        return None;
    }
    let mut bytes = [0u8; MESSAGE_BYTES];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = word(HEAD_WORDS + i / 4).to_le_bytes().get(i % 4).copied().unwrap_or(0);
    }
    let report = Report {
        count: word(1),
        len: (word(3) as usize).min(MESSAGE_BYTES),
        bytes,
    };
    Some((report, word(2) == 1))
}

/// The last three parts of a source path - `embedded-tls-0.19.0/src/pki.rs`,
/// `src/pins/configs/mqtt.rs` - whichever separator the build machine used.
fn short_path(file: &str) -> &str {
    let mut cut = file.len();
    for _ in 0..3 {
        match file.get(..cut).and_then(|head| head.rfind(['/', '\\'])) {
            Some(i) => cut = i,
            None => return file,
        }
    }
    file.get(cut + 1..).unwrap_or(file)
}

/// The message as it is written: a fixed buffer, cut on a character.
struct Text {
    bytes: [u8; MESSAGE_BYTES],
    len: usize,
}

impl Text {
    const fn new() -> Self {
        Self {
            bytes: [0; MESSAGE_BYTES],
            len: 0,
        }
    }

    /// `false` once it is full: the rest is cut.
    fn push_str(&mut self, s: &str) -> bool {
        for c in s.chars() {
            let mut buf = [0u8; 4];
            let encoded = c.encode_utf8(&mut buf).as_bytes();
            let end = self.len + encoded.len();
            let Some(slot) = self.bytes.get_mut(self.len..end) else {
                return false;
            };
            slot.copy_from_slice(encoded);
            self.len = end;
        }
        true
    }

    /// A path, with `/` whatever the build machine wrote.
    fn push_path(&mut self, path: &str) {
        for c in path.chars() {
            let mut buf = [0u8; 4];
            if !self.push_str(if c == '\\' { "/" } else { c.encode_utf8(&mut buf) }) {
                return;
            }
        }
    }

    fn push_u32(&mut self, mut n: u32) {
        let mut digits = [0u8; 10];
        let mut i = digits.len();
        loop {
            i -= 1;
            if let Some(d) = digits.get_mut(i) {
                *d = b'0' + (n % 10) as u8;
            }
            n /= 10;
            if n == 0 || i == 0 {
                break;
            }
        }
        if let Some(Ok(s)) = digits.get(i..).map(core::str::from_utf8) {
            self.push_str(s);
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(self.bytes.get(..self.len).unwrap_or(&[])).unwrap_or("")
    }
}

impl core::fmt::Write for Text {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        if self.push_str(s) {
            Ok(())
        } else {
            Err(core::fmt::Error)
        }
    }
}
