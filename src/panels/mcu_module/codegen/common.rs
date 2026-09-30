// ── Section markers ───────────────────────────────────────────────────────────
//
// The GEN_BEGIN … GEN_END block is auto-generated and replaced whenever the
// pin configuration changes.  It includes the HAL use items, any peripheral
// helper functions, the #[entry] attribute, and the opening of fn main().
// The block is intentionally left open — USER_TAIL closes main() with the
// user-editable loop body, which is preserved across every regen.

pub const GEN_BEGIN: &str = "// <<< GENERATED BEGIN — do not edit between these markers >>>";
pub const GEN_END: &str = "// <<< GENERATED END >>>";

use super::super::mcu::Mcu;

/// The devices the user grouped, as a comment at the top of the generated
/// block.
///
/// A sensor is three pads that belong together, and the generated file had no
/// way to say so: the bindings come out ordered by pad, so a UART pair and the
/// spare input line beside it end up wherever the chip's pin numbering puts
/// them. This gathers each device into one place to read.
///
/// A COMMENT and nothing else. The group name is deliberately kept out of every
/// identifier: a name spliced into a binding is re-parsed as part of the pin's
/// label when the project is reopened, and doubles - `pa3_in_radar_pulse`
/// becomes `pa3_in_radar_radar_pulse` on the next open. And any generated name
/// that moves with the grouping breaks the user's own code, because only the
/// text between the markers is ever rewritten.
///
/// It sits INSIDE the markers, at the TOP of the file where the block starts
/// (the markers wrap the whole generated preamble, not the body of `main`), so
/// it is rebuilt on every save and a device renamed in the panel is renamed
/// here too.
pub fn device_comment(mcu: &Mcu) -> String {
    let live: Vec<&crate::panels::mcu_module::mcu_config::PinGroup> =
        mcu.groups.iter().filter(|g| g.is_live()).collect();
    if live.is_empty() {
        return String::new();
    }
    let mut lines = String::new();
    for g in live {
        let pads: Vec<String> = g
            .pins
            .iter()
            .filter_map(|n| mcu.find_pin(*n))
            .map(|p| {
                let what = p.selected_function.short_label();
                if matches!(p.selected_function, PinFunction::Unset) {
                    p.name.clone()
                } else {
                    format!("{} ({what})", p.name)
                }
            })
            .collect();
        // The devices of an I2C bus put in this Device BY HAND - only those:
        // one that is in it because its bus is was never stored anywhere, and
        // listing it would change the file of every project that grouped a
        // bus the moment it was reopened.
        let devices: Vec<String> = g
            .i2c
            .iter()
            .filter_map(|(inst, uid)| i2c_device_label(mcu, *inst, *uid))
            .collect();
        let parts: Vec<String> = pads.into_iter().chain(devices).collect();
        if !parts.is_empty() {
            // Trimmed, like `mcu.config` writes it - otherwise a name the user
            // left a space on reads "// radar : GP0" here and "radar" there.
            lines.push_str(&format!("// {}: {}\n", g.name.trim(), parts.join(", ")));
        }
    }
    // A Device can be live with nothing to list: its only member a device of
    // an I2C bus that was removed since (kept so an undo brings it back). A
    // header over no line would say nothing.
    if lines.is_empty() {
        return String::new();
    }
    format!("// ── Devices on this board ──\n{lines}\n")
}

/// How an I2C device reads in the device comment: `I2C1 oled @ 0x3C` - the bus
/// named the way this family names it (`TWIM0` on an nRF, like its address
/// consts), the device by its name or its place in the list. `None` for a
/// device that is gone.
fn i2c_device_label(mcu: &Mcu, instance: u8, uid: u32) -> Option<String> {
    use crate::panels::mcu_module::modules::{I2cDeviceKey, ModuleConfig, ModuleKind};
    let cfg = mcu.modules.iter().find_map(|m| match &m.config {
        ModuleConfig::I2c(c)
            if m.kind == ModuleKind::GenericInterfaceI2c && c.instance == instance =>
        {
            Some(c)
        }
        _ => None,
    })?;
    let at = cfg.position(I2cDeviceKey::Uid(uid))?;
    let d = &cfg.devices[at];
    let bus = if super::nrf::is_nrf(&mcu.family) {
        format!("TWIM{instance}")
    } else {
        format!("I2C{instance}")
    };
    // A name is typed text: nothing in it may end the comment line early.
    let name: String = d
        .name
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let name = if name.is_empty() {
        format!("device {}", at + 1)
    } else {
        name
    };
    Some(format!("{bus} {name} @ 0x{:02X}", d.address))
}

/// Put [`device_comment`] just inside the generated block.
///
/// One insertion point for every backend: they all funnel through
/// `Mcu::fresh_main_rs` and `Mcu::update_main_rs`, so the six of them do not
/// each need to remember. A file with no block (the ESP scheme, or a family
/// with no backend) is returned untouched.
pub fn with_device_comment(code: String, mcu: &Mcu) -> String {
    let block = device_comment(mcu);
    if block.is_empty() {
        return code;
    }
    let Some(i) = code.find(GEN_BEGIN) else {
        return code;
    };
    let after = i + GEN_BEGIN.len();
    // After the marker AND its newline, so the marker keeps its own line.
    let at = match code[after..].find('\n') {
        Some(nl) => after + nl + 1,
        None => return code,
    };
    let mut out = String::with_capacity(code.len() + block.len());
    out.push_str(&code[..at]);
    out.push_str(&block);
    out.push_str(&code[at..]);
    out
}

// ── MCU identity marker ───────────────────────────────────────────────────────
//
// Written into the invariant file header (above GEN_BEGIN, so it survives every
// re-splice). Lets a reopened project restore the *exact* chip it was created
// with — including user-imported chips that share a HAL crate with a built-in
// (e.g. an imported "esp32c3-graph" vs the built-in "esp32c3"), which the
// Cargo.toml `hal_dep` sniff alone cannot tell apart.

pub const MCU_ID_MARKER: &str = "// rust_on_chip:mcu=";

/// The marker before the 2026-09-20 rename. Every project created until then
/// carries it, and most backends never rewrite the header above GEN_BEGIN (the
/// embassy STM32 ones rebuild it, and so move it to the new spelling on the next
/// Save) — a project not re-saved keeps it for good, so it is read FOREVER.
/// Without it a project on an imported chip would fall back to the first chip
/// sharing its HAL crate and be regenerated for the wrong part on Save.
pub const LEGACY_MCU_ID_MARKER: &str = "// embedded-ide:mcu=";

/// First line of every generated `main.rs`.
pub const AUTOGEN_BANNER: &str = "// Auto-generated by RustOnChip";

/// The banner before the rename. Existing projects keep it (most backends leave
/// the header above the generated block as written); the ESP backend still
/// anchors on it.
pub const LEGACY_AUTOGEN_BANNER: &str = "// Auto-generated by Embedded IDE";

/// The header line that records the MCU id, or an empty string when the id is
/// unknown (so older/unidentified projects emit nothing).
pub fn mcu_id_marker_line(id: &str) -> String {
    if id.is_empty() {
        String::new()
    } else {
        format!("{MCU_ID_MARKER}{id}\n")
    }
}

/// Extract the MCU id recorded by [`mcu_id_marker_line`], if present — in the
/// current spelling or the one written before the rename.
pub fn parse_mcu_id(source: &str) -> Option<String> {
    source.lines().find_map(|l| {
        let l = l.trim();
        l.strip_prefix(MCU_ID_MARKER)
            .or_else(|| l.strip_prefix(LEGACY_MCU_ID_MARKER))
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty())
    })
}

// ── Hand-written clock block ──────────────────────────────────────────────────
//
// The clock setup sits INSIDE the generated section, so it is normally replaced
// on every regeneration like everything else there. A chip whose family has no
// RCC recipe cannot have that setup generated at all, though — the tree in the
// Clock tab has nothing to be turned into. For those the block is written by
// hand, and these markers carve out the one region of the generated section that
// survives a regen.
//
// They are emitted ONLY in manual mode, so every project that lets the IDE
// generate its clock keeps exactly the output it had.

pub const CLOCK_BEGIN: &str = "    // <<< CLOCK BEGIN — hand-written, kept across regeneration >>>";
pub const CLOCK_END: &str = "    // <<< CLOCK END >>>";

/// The hand-written clock region of `source`, markers included.
pub fn clock_region(source: &str) -> Option<&str> {
    let begin = source.find(CLOCK_BEGIN)?;
    let end = source[begin..].find(CLOCK_END)? + begin + CLOCK_END.len();
    Some(&source[begin..end])
}

/// Carry the user's hand-written clock block from `existing` into `section`.
///
/// Only in manual mode, and only when both sides actually have the region:
/// - not manual → `section` is returned untouched, so generated projects are
///   byte-for-byte what they were;
/// - manual but the old file has no region → the freshly generated block stays,
///   which is exactly the seed the user then edits;
/// - manual and both have one → the user's text wins.
pub fn keep_manual_clock(existing: &str, section: String, manual: bool) -> String {
    if !manual {
        return section;
    }
    let (Some(old), Some(new)) = (clock_region(existing), clock_region(&section)) else {
        return section;
    };
    section.replace(new, old)
}

/// Join generated statements so ONE blank line separates each from the next.
///
/// A "statement" here is usually two lines — an `#[allow(unused_mut,
/// unused_variables)]` and the `let` it guards — and a column of those run
/// together reads as an unbroken wall: the attribute of the next pin sits
/// directly under the previous pin's code, so nothing tells the eye where one
/// pin ends and the next begins. The blank line turns the wall back into a list.
///
/// No blank is left after the LAST item — the caller's own section separator
/// follows, and two blank lines in a row is just the wall again with holes.
/// Items that do not already end in a newline get one, so a caller can pass
/// either shape.
pub fn blank_separated<I: IntoIterator<Item = String>>(items: I) -> String {
    let mut out = String::new();
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&item);
        if !item.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

// ── User tail — closes fn main() ─────────────────────────────────────────────
//
// Written once on first generation; the loop body is user-editable and is
// preserved across every pin-configuration change.

pub const USER_TAIL: &str = "    loop {\n        // Your main loop code here.\n    }\n}\n";

/// The same tail for an ASYNC (embassy-executor) runtime, opening with the one
/// warning that costs a beginner a whole afternoon.
///
/// Every async backend here — STM32 embassy, embassy-rp, and ESP on esp-rtos —
/// runs embassy-executor, whose scheduler is COOPERATIVE: a task only yields at
/// an `.await`. A `loop` with no await in it therefore never gives the executor
/// control back, and every other spawned task (the EXTI edge watchers, the
/// buffered-UART pump, the radio driver) simply never runs again. The symptom is
/// a program that looks like it hung for no reason, and nothing in the compiler
/// output points at it.
///
/// It also ENDS with an await, so the loop the IDE generates is already a legal
/// cooperative loop rather than the exact mistake the comment above it warns
/// about. Sixty seconds is a placeholder long enough to read as one — nothing
/// paces itself at one minute — so it invites being changed instead of being
/// mistaken for a considered value.
///
/// Written FULLY QUALIFIED (`embassy_time::Timer`, `embassy_time::Duration`)
/// with no `use` line: the tail lives in the user's editable region, and an
/// import in the invariant header is a second edit somewhere else that a later
/// regeneration or a delete-this-line would leave dangling. `embassy-time` is on
/// every async project — `ensure_async_deps` adds it for all three flavours —
/// so the path always resolves.
///
/// `concat!` rather than a `\`-continued literal on purpose: rustfmt joins those
/// and leaves a run of spaces inside the string (see the notes on
/// `rustfmt-joins-continued-strings`), which would land in the user's file.
pub const ASYNC_USER_TAIL: &str = concat!(
    "    loop {\n",
    "        /* !!! IMPORTANT !!!\n",
    "           Every iteration must `.await` — Embassy tasks are cooperative, and a\n",
    "           non-awaiting loop blocks all other tasks from ever running.\n",
    "        */\n",
    "\n",
    "        // Your main loop code here.\n",
    "\n",
    "        embassy_time::Timer::after(embassy_time::Duration::from_millis(60000)).await;\n",
    "    }\n",
    "}\n",
);

/// Swap a still-PRISTINE user tail for the one this runtime wants, when a
/// project is re-generated after a Blocking/Async runtime switch.
///
/// The tail below `GEN_END` belongs to the user and every splice preserves it
/// verbatim — which is why a runtime switch would otherwise never show the async
/// warning (the file already exists, so `fresh_main_rs` never runs again), and
/// why switching back would leave a warning about awaiting in a program that has
/// no executor. Both are fixed by exchanging the tail ONLY while it is still
/// character-for-character the seed we wrote: the moment the user types a single
/// line in there it is theirs, and it is left alone.
///
/// Leading newlines are preserved so the blank line between `GEN_END` and `loop`
/// does not drift on either side of the swap.
///
/// The seed may also be followed by the user's own items: a file that came from
/// RTIC keeps its helpers AFTER the seed ([`tail_leaving_rtic`]), and anyone can
/// add a `fn` below `main`. The seed is still ours then, so it is exchanged and
/// what follows it is left exactly as it was.
pub fn retarget_pristine_tail(after: &str, want_async: bool) -> String {
    let (from, to) = if want_async {
        (USER_TAIL, ASYNC_USER_TAIL)
    } else {
        (ASYNC_USER_TAIL, USER_TAIL)
    };
    let body = after.trim_start_matches('\n');
    let lead = &after[..after.len() - body.len()];
    if body.trim() == from.trim() {
        return format!("{lead}{to}");
    }
    match body.strip_prefix(from) {
        Some(rest) => format!("{lead}{to}{rest}"),
        None => after.to_owned(),
    }
}

/// Fit a kept `main.rs` header to the runtime now writing the file.
///
/// The header above the markers survives a re-splice, because a user may have
/// added `mod`s and `use`s there. Two things in it belong to the RUNTIME,
/// though, and after a switch they are the old one's: the `// MCU: … | HAL: …`
/// label, and `use cortex_m_rt::entry;` - which Blocking and Native need and
/// RTIC must not have (its macro writes `fn main`, so the import is dead). Both
/// follow `own`, the header this runtime writes; every other line stays.
///
/// An Async header is not refitted but replaced, by the callers: it has more
/// runtime lines than these two, and the Async splice rebuilds its own anyway.
/// The callers refit only across a switch to or from RTIC; on every other
/// splice the header is kept verbatim, as it always was.
///
/// The import is ADDED only when no form of it is there already: a user who
/// wrote `use cortex_m_rt::{entry, exception};` for a fault handler has it, and
/// a second one is E0252. It is REMOVED only as the exact line the generator
/// writes - any other form is the user's, and at worst an unused import.
pub fn refit_header(head: &str, own: &str) -> String {
    const ENTRY: &str = "use cortex_m_rt::entry;";
    let label = |h: &str| {
        h.lines()
            .find(|l| l.starts_with("// MCU:"))
            .map(str::to_owned)
    };
    let mut out = head.to_owned();
    if let (Some(theirs), Some(ours)) = (label(head), label(own))
        && theirs != ours
    {
        out = out.replacen(&theirs, &ours, 1);
    }
    let seed_line = |h: &str| h.lines().any(|l| l.trim() == ENTRY);
    if seed_line(own) {
        if !imports_entry(&out) {
            // Where the generator puts it: right under the panic handler.
            let anchor = "use panic_halt as _;\n";
            let at = match out.find(anchor) {
                Some(at) => at + anchor.len(),
                None => out.trim_end_matches('\n').len() + 1,
            };
            out.insert_str(at.min(out.len()), &format!("{ENTRY}\n"));
        }
    } else if seed_line(&out) {
        out = out.replacen(&format!("{ENTRY}\n"), "", 1);
    }
    out
}

/// Does this header bring cortex-m-rt's `entry` into scope, in any form a
/// person writes it: the plain line, a brace list (over several lines too), a
/// glob, or an `x as entry` rename, with or without a trailing comment.
fn imports_entry(head: &str) -> bool {
    const USE: &str = "use cortex_m_rt::";
    // Line comments out first, so a commented-out import does not count and a
    // trailing comment does not hide a real one.
    let code: Vec<&str> = head
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect();
    let code = code.join("\n");
    let mut rest = code.as_str();
    while let Some(at) = rest.find(USE) {
        let path_start = at + USE.len();
        let end = rest[path_start..]
            .find(';')
            .map_or(rest.len(), |e| path_start + e);
        let words: Vec<&str> = rest[path_start..end]
            .split(|c: char| c == ',' || c == '{' || c == '}' || c.is_whitespace())
            .filter(|w| !w.is_empty())
            .collect();
        // `entry as other` puts `other` in scope, not `entry`; `x as entry`
        // ends on `entry` and counts.
        let named = words.iter().enumerate().any(|(i, w)| match *w {
            "*" => true,
            "entry" => words.get(i + 1) != Some(&"as"),
            _ => false,
        });
        if named {
            return true;
        }
        rest = &rest[end..];
    }
    false
}

/// Did the RTIC runtime write this file's generated section?
///
/// RTIC generates its whole `#[rtic::app] mod app { … }` between the markers,
/// so the tail below them closes nothing. Every other runtime opens its entry
/// fn inside the markers and closes it in the tail - which is why the tail of
/// a file leaving RTIC needs [`tail_leaving_rtic`].
pub fn section_is_rtic(existing: &str) -> bool {
    match (existing.find(GEN_BEGIN), existing.find(GEN_END)) {
        (Some(begin), Some(end)) if begin < end => existing[begin..end].contains("#[rtic::app"),
        _ => false,
    }
}

/// The tail of a file leaving RTIC, for a runtime whose entry fn the tail has
/// to close. RTIC's own seed is swapped for `seed`. A tail the user wrote in
/// holds module-level helpers, so it is kept AFTER a fresh `seed` that closes
/// the entry - moving it anywhere else would put `fn`s inside `fn main`.
pub fn tail_leaving_rtic(after: &str, seed: &str) -> String {
    if after.trim() == super::rtic::RTIC_USER_TAIL.trim() {
        return seed.to_owned();
    }
    format!("{seed}\n{after}")
}

// ── Strict-lints exemption for generated code ─────────────────────────────────
//
// When the MCU System "Strict lints" toggle is on, the project Cargo.toml gets a
// `[lints.clippy]` deny profile (see `project_gen::ensure_strict_lints`). The
// GENERATED code (main's init, the peripheral `configs/*.rs`) uses `unwrap()`,
// `as`, indexing, … idiomatically, so it is exempted with `#[allow]` — leaving
// only the USER's own code (their modules, and main's loop below the GEN block —
// no, the whole entry fn is exempt since its init bindings must stay in scope)
// under the strict lints.

/// The strict-profile clippy lints as `#[allow]`-able names. Matches the deny
/// list in `project_gen::STRICT_LINTS_BLOCK`.
const STRICT_LINT_LIST: &str = "clippy::pedantic, clippy::nursery, \
     clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, \
     clippy::arithmetic_side_effects, clippy::unreachable, clippy::unimplemented, \
     clippy::unchecked_time_subtraction, clippy::todo, clippy::string_slice, \
     clippy::panic_in_result_fn, clippy::panic, clippy::exit, clippy::as_conversions";

/// Every entry attribute the backends emit, matched against a whole line.
///
/// `#[esp_rtos::main]` was missing, so an ESP project on the ASYNC runtime got
/// no exemption at all and its generated init - the `take().unwrap()`s and `as`
/// casts this exists for - was linted in full, inside a GENERATED block the
/// user cannot edit. Blocking was fine, which is why it went unseen: the two
/// ESP runtimes use different attributes.
///
/// The same again, two backends later: the micro:bit's Blocking block opens on
/// `#[cortex_m_rt::entry]` and the Pico's on `#[rp2040_hal::entry]` /
/// `#[rp235x_hal::entry]`, written out in full where the STM32 one imports
/// `entry` and writes it bare. Async was fine on both, for the same reason as
/// before.
///
/// Only what a backend emits today. A bare `#[main]` sat here from the first
/// version and nothing has written it since; the test below holds the list to
/// the generators in both directions, so a spelling cannot linger either.
const ENTRY_ATTRIBUTES: [&str; 7] = [
    "#[entry]",
    "#[cortex_m_rt::entry]",
    "#[rp2040_hal::entry]",
    "#[rp235x_hal::entry]",
    "#[embassy_executor::main]",
    "#[esp_hal::main]",
    "#[esp_rtos::main]",
];

/// When `strict`, put `#[allow(<strict lints>)]` on the generated entry fn so
/// its init (`take().unwrap()`, `as` casts, …) doesn't flood clippy. Inserted
/// just before `#[entry]` / `#[embassy_executor::main]` — inside the GEN block,
/// so it's rebuilt on every regeneration (no accumulation). The whole `main` is
/// exempt: its init bindings must stay in scope for the user's loop, so the loop
/// can't be split off; the user's real code lives in their own modules, which
/// stay fully linted. No-op when `strict` is off or no entry attr is found.
pub fn strict_main_exemption(code: String, strict: bool) -> String {
    if !strict {
        return code;
    }
    for entry in ENTRY_ATTRIBUTES {
        let mut offset = 0;
        for line in code.split_inclusive('\n') {
            if line.trim() == entry {
                let attr = format!("#[allow({STRICT_LINT_LIST})]\n");
                let mut out = String::with_capacity(code.len() + attr.len());
                out.push_str(&code[..offset]);
                out.push_str(&attr);
                out.push_str(&code[offset..]);
                return out;
            }
            offset += line.len();
        }
    }
    code
}

/// When `strict`, put a module-level `#![allow(<strict lints>)]` right after a
/// config file's `// <<< GENERATED>>>` marker (before the first `const`), so the
/// whole generated peripheral module is exempt. Inside the marker block, so
/// `sync_config_files` re-splices it in/out on toggle. No-op otherwise.
pub fn strict_config_exemption(body: String, strict: bool) -> String {
    const MARK: &str = "// <<< GENERATED>>>";
    if !strict {
        return body;
    }
    let Some(pos) = body.find(MARK) else {
        return body;
    };
    let after = pos + MARK.len();
    let insert_at = body[after..]
        .find('\n')
        .map(|n| after + n + 1)
        .unwrap_or(after);
    let attr = format!("#![allow({STRICT_LINT_LIST})]\n");
    let mut out = String::with_capacity(body.len() + attr.len());
    out.push_str(&body[..insert_at]);
    out.push_str(&attr);
    out.push_str(&body[insert_at..]);
    out
}

// ── Virtual-module data models ────────────────────────────────────────────────

use super::super::modules::{I2cModuleConfig, VirtualModule};

fn indent_block(s: &str) -> String {
    s.lines()
        .map(|l| {
            if l.trim().is_empty() {
                "\n".to_owned()
            } else {
                format!("    {l}\n")
            }
        })
        .collect()
}

/// Append each module's RX/TX data model as an inline `mod <id> { … }` at the end
/// of `main.rs` (family-agnostic). Additive: a module already present (matched by
/// `mod <id>`) is left untouched, so edits survive every regeneration — and a
/// module with an empty data model emits nothing. The module's id is a valid Rust
/// identifier (e.g. `_usart_1`), so its types are reachable as `_usart_1::…`.
pub fn ensure_module_models(mut file: String, modules: &[VirtualModule]) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for m in modules {
        let (rx, tx) = (m.config.rx_model(), m.config.tx_model());
        if rx.trim().is_empty() && tx.trim().is_empty() {
            continue;
        }
        if file.contains(&format!("mod {} ", m.id)) || file.contains(&format!("mod {}{{", m.id)) {
            continue;
        }
        let mut body = String::new();
        if !rx.trim().is_empty() {
            body.push_str("    // ── RX data model ──\n");
            body.push_str(&indent_block(rx));
        }
        if !tx.trim().is_empty() {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str("    // ── TX data model ──\n");
            body.push_str(&indent_block(tx));
        }
        blocks.push(format!(
            "\n// Data model for {} (editable — kept across regeneration)\nmod {} {{\n{body}}}\n",
            m.name, m.id,
        ));
    }
    if blocks.is_empty() {
        return file;
    }
    if !file.ends_with('\n') {
        file.push('\n');
    }
    for b in blocks {
        file.push_str(&b);
    }
    file
}

// ── I2C device address ────────────────────────────────────────────────────────

/// The module's 7-bit device address, as the `pub const` an I2C config file
/// carries. Ends in a newline; empty string for a family that generates no bus.
///
/// One emitter for every backend, because this is the setting that reached only
/// ONE of them. The panel has collected an address since the I2C module existed,
/// and `I2cModuleConfig.address` had a single non-UI reader in the whole tree —
/// the ESP file — so on every other family the user set an address, saved, and
/// the generated code never mentioned it again. That is the house rule
/// backwards: the UI offered what codegen would not write.
///
/// The comment carries as much weight as the value. Every HAL here takes the
/// address PER TRANSACTION (`write_read(addr, …)`) and none takes it at `init`,
/// so a reader who assumes it is a constructor argument goes hunting for a
/// parameter that does not exist. Each generated example used to invent its own
/// `const ADDR: u8 = 0x3C;` two lines below `init` rather than name the real
/// one — which meant the example ignored the address the user had just set.
///
/// Sits INSIDE the GENERATED markers: it follows the Virtual Module, while the
/// editable half below them stays the user's.
///
/// `peri` is `None` in a per-peripheral config file, whose module path
/// (`pins::configs::i2c1`) already says which bus this is. It is the peripheral's
/// name in `main.rs`, where the families that build their buses inline put it:
/// one namespace holds every instance at once, so the name has to carry the bus
/// or two of them would collide on it. The CALLER spells the peripheral, because
/// the families do not agree on it — Nordic's I2C is `TWIM0`, and a const called
/// `I2C0_…` beside a `Twim::new` would name a peripheral that chip has not got.
pub fn device_address_const(peri: Option<&str>, address: u8) -> String {
    let name = match peri {
        None => "DEVICE_ADDRESS".to_owned(),
        Some(p) => format!("{p}_DEVICE_ADDRESS"),
    };
    let mut s = String::new();
    s.push_str("// 7-bit address of the device on this bus — for YOUR code, not for `init`:\n");
    s.push_str("// an I2C master takes the address per transaction.\n");
    // 0x00 is the general-call address, not a device — and it is what an
    // untouched module still holds (`I2cModuleConfig::new`). Saying so beats
    // emitting it as though someone had chosen it: the const stays, so user
    // code referring to it keeps compiling once the address IS set.
    if address == 0 {
        s.push_str("// Not set in the IDE yet: 0x00 is the general-call address, not a device.\n");
    }
    s.push_str(&format!("pub const {name}: u8 = 0x{address:02X};\n"));
    s
}

#[cfg(test)]
mod device_address_tests {
    use super::super::super::modules::{I2cDevice, I2cModuleConfig};
    use super::device_address_const;

    /// Inside a config file the module path already names the bus, so the const
    /// is the bare name every family spells the same way. In `main.rs` it is
    /// not: one namespace holds every instance, and the families do not even
    /// agree on what the peripheral is called.
    #[test]
    fn the_name_carries_the_bus_only_where_one_namespace_holds_them_all() {
        assert!(device_address_const(None, 0x3C).contains("pub const DEVICE_ADDRESS: u8 = 0x3C;"));
        assert!(
            device_address_const(Some("I2C0"), 0x3C)
                .contains("pub const I2C0_DEVICE_ADDRESS: u8 = 0x3C;")
        );
        // Nordic's I2C peripheral is a TWIM, and the const sits beside a
        // `Twim::new`. An `I2C0_` there would name a block the chip has not got.
        assert!(
            device_address_const(Some("TWIM0"), 0x3C)
                .contains("pub const TWIM0_DEVICE_ADDRESS: u8 = 0x3C;")
        );
    }

    /// An untouched module still holds 0x00, which is the general-call address
    /// and not a device. The const is emitted anyway - user code naming it must
    /// keep compiling once the address IS set - but it says what it is.
    #[test]
    fn an_unset_address_says_so_rather_than_passing_for_a_choice() {
        let unset = device_address_const(None, 0);
        assert!(
            unset.contains("pub const DEVICE_ADDRESS: u8 = 0x00;"),
            "{unset}"
        );
        assert!(unset.contains("Not set in the IDE yet"), "{unset}");
        let set = device_address_const(None, 0x3C);
        assert!(!set.contains("Not set in the IDE yet"), "{set}");
    }

    fn device(name: &str, address: u8) -> I2cDevice {
        I2cDevice {
            name: name.into(),
            address,
            ..Default::default()
        }
    }

    fn names(files: &[(String, String)]) -> Vec<&str> {
        files.iter().map(|(n, _)| n.as_str()).collect()
    }

    fn body<'a>(files: &'a [(String, String)], name: &str) -> &'a str {
        &files.iter().find(|(n, _)| n == name).expect(name).1
    }

    /// A bus is a folder: its `mod.rs` declares one module per device, and
    /// each device - the ONLY one included - has a file carrying its address.
    /// The bus itself no longer names an address.
    #[test]
    fn a_bus_is_a_folder_with_a_file_per_device() {
        let mut c = I2cModuleConfig::new(1);
        c.devices = vec![device("imu", 0x68)];
        let files = super::i2c_bus_files("i2c1", super::i2c_device_mods(Some(&c)), Some(&c));
        assert_eq!(names(&files), vec!["i2c1/mod.rs", "i2c1/device1_imu.rs"]);
        assert_eq!(
            body(&files, "i2c1/mod.rs")
                .matches("pub mod device1_imu;")
                .count(),
            1
        );
        assert!(!body(&files, "i2c1/mod.rs").contains("DEVICE_ADDRESS: u8"));
        let dev = body(&files, "i2c1/device1_imu.rs");
        assert!(
            dev.contains("pub const DEVICE_ADDRESS: u8 = 0x68;"),
            "{dev}"
        );
        assert!(dev.contains("Device #1 on I2C1: imu"), "{dev}");

        c.devices.push(device("SSD1306 display", 0x3C));
        let files = super::i2c_bus_files("i2c1", String::new(), Some(&c));
        assert_eq!(
            names(&files),
            vec![
                "i2c1/mod.rs",
                "i2c1/device1_imu.rs",
                "i2c1/device2_ssd1306_display.rs"
            ]
        );
        let second = body(&files, "i2c1/device2_ssd1306_display.rs");
        assert!(second.contains("= 0x3C;"), "{second}");
        assert!(
            !second.contains("0x68"),
            "a device must not carry its neighbour's address"
        );
    }

    /// The legacy single address is device 1, as on the canvas - a bus from
    /// before device lists gets its file too. No device: `mod.rs` alone, saying
    /// where they will go.
    #[test]
    fn the_legacy_address_is_device_one_and_no_device_is_a_bare_folder() {
        let mut c = I2cModuleConfig::new(0);
        c.address = 0x3C;
        let files = super::i2c_bus_files("twim0", String::new(), Some(&c));
        assert_eq!(names(&files), vec!["twim0/mod.rs", "twim0/device1.rs"]);
        assert!(body(&files, "twim0/device1.rs").contains("= 0x3C;"));
        assert!(super::i2c_device_mods(Some(&c)).contains("pub mod device1;\n"));

        c.address = 0;
        assert_eq!(
            names(&super::i2c_bus_files("twim0", String::new(), Some(&c))),
            vec!["twim0/mod.rs"]
        );
        let none = super::i2c_device_mods(Some(&c));
        assert!(none.contains("No device on this bus yet"), "{none}");
        assert!(!none.contains("pub mod"), "{none}");
        assert_eq!(super::i2c_device_mods(None), none);
        assert!(none.ends_with('\n') && super::i2c_device_mods(Some(&c)).ends_with('\n'));
    }

    /// The number keeps names apart - three devices called the same get three
    /// files, and no `_2` - and an unnamed device is just its number.
    #[test]
    fn the_number_keeps_the_names_apart() {
        let mut c = I2cModuleConfig::new(1);
        c.devices = vec![
            device("sensor", 0x40),
            device("sensor", 0x41),
            device("", 0x42),
            device("Sensor!", 0x43),
        ];
        let files = super::i2c_bus_files("i2c1", String::new(), Some(&c));
        assert_eq!(
            names(&files),
            vec![
                "i2c1/mod.rs",
                "i2c1/device1_sensor.rs",
                "i2c1/device2_sensor.rs",
                "i2c1/device3.rs",
                "i2c1/device4_sensor.rs"
            ]
        );
        assert!(body(&files, "i2c1/device3.rs").contains("Device #3 on I2C1: device 3"));
    }

    /// The editable half is the same text in every device file: it is kept
    /// verbatim when the file is renamed, so nothing in it may name the device.
    #[test]
    fn the_editable_half_names_no_device() {
        let tail = |f: &str| f[f.find("GENERATED END").unwrap()..].to_owned();
        let a = super::I2cDeviceFile {
            k: 1,
            stem: "device1_oled".into(),
            shown: "oled".into(),
            address: 0x3C,
            uid: 4,
        };
        let b = super::I2cDeviceFile {
            k: 7,
            stem: "device7".into(),
            shown: "device 7".into(),
            address: 0,
            uid: 0,
        };
        assert_eq!(
            tail(&super::i2c_device_file("i2c1", &a)),
            tail(&super::i2c_device_file("twim0", &b))
        );
    }

    /// What `sync_config_files` reads back is what the device file writes -
    /// including after the strict-lints attribute went in - and only from the
    /// generated block. No uid, no id line.
    #[test]
    fn a_device_file_reads_back() {
        let d = super::I2cDeviceFile {
            k: 2,
            stem: "device2_imu".into(),
            shown: "imu".into(),
            address: 0x68,
            uid: 7,
        };
        for strict in [false, true] {
            let f = super::strict_config_exemption(super::i2c_device_file("i2c1", &d), strict);
            assert_eq!(super::device_file_address(&f), Some(0x68), "{f}");
            assert_eq!(super::device_file_id(&f), Some(("i2c1", 7)), "{f}");
        }
        let unminted = super::i2c_device_file(
            "i2c1",
            &super::I2cDeviceFile {
                uid: 0,
                ..d.clone()
            },
        );
        assert!(!unminted.contains("device-id"), "{unminted}");
        assert_eq!(super::device_file_id(&unminted), None);
        // Below the markers is the user's, whatever it says.
        let fake =
            format!("{unminted}\n// device-id: i2c1/9\npub const DEVICE_ADDRESS: u8 = 0x11;\n");
        assert_eq!(super::device_file_id(&fake), None);
        assert_eq!(super::device_file_address(&fake), Some(0x68));

        assert_eq!(
            super::parse_device_file_name("device3_imu.rs"),
            Some((3, "imu"))
        );
        assert_eq!(super::parse_device_file_name("device12.rs"), Some((12, "")));
        assert_eq!(super::parse_device_file_name("device_imu.rs"), None);
        assert_eq!(super::parse_device_file_name("device1_.rs"), None);
        assert_eq!(super::parse_device_file_name("mod.rs"), None);
        assert_eq!(super::parse_device_file_name("device1"), None);
    }

    /// The names the RP/nRF async consts are built from, which are also the
    /// file names of a project from before buses were folders - unchanged.
    #[test]
    fn the_legacy_stems_are_unchanged() {
        let mut c = I2cModuleConfig::new(1);
        c.devices = vec![device("imu", 0x68)];
        assert!(
            super::legacy_i2c_device_stems("i2c1", &c).is_empty(),
            "one device: none"
        );
        c.devices = vec![device("SSD1306 display", 0x3C), device("imu", 0x68)];
        let stems: Vec<String> = super::legacy_i2c_device_stems("i2c1", &c)
            .into_iter()
            .map(|(s, ..)| s)
            .collect();
        assert_eq!(stems, vec!["i2c1_ssd1306_display", "i2c1_imu"]);
        c.devices = vec![
            device("sensor", 0x40),
            device("sensor", 0x41),
            device("", 0x42),
        ];
        let stems: Vec<String> = super::legacy_i2c_device_stems("twim0", &c)
            .into_iter()
            .map(|(s, ..)| s)
            .collect();
        assert_eq!(
            stems,
            vec!["twim0_sensor", "twim0_sensor_2", "twim0_device3"]
        );
    }

    /// Read back off the new file names, the old names come out the same as
    /// the rule that wrote them - repeats, unnamed devices and all.
    #[test]
    fn old_names_are_rebuilt_from_the_new_ones() {
        let mut c = I2cModuleConfig::new(1);
        c.devices = vec![
            device("sensor", 0x40),
            device("", 0),
            device("sensor", 0x41),
            device("SSD1306 display", 0x3C),
            device("", 0),
        ];
        let old: Vec<String> = super::legacy_i2c_device_stems("i2c1", &c)
            .into_iter()
            .map(|(s, ..)| s)
            .collect();
        let names: Vec<String> = super::i2c_device_files_of(&c)
            .iter()
            .map(|d| format!("{}.rs", d.stem))
            .collect();
        let read_back: Vec<(usize, &str)> = names
            .iter()
            .map(|n| super::parse_device_file_name(n).unwrap())
            .collect();
        assert_eq!(super::legacy_device_stems_for("i2c1", &read_back), old);
    }

    /// The comment is the half that stops a reader hunting for an `init`
    /// parameter that does not exist: every HAL here takes the address per
    /// transaction.
    #[test]
    fn the_const_explains_that_it_is_not_an_init_argument() {
        let out = device_address_const(None, 0x3C);
        assert!(out.contains("per transaction"), "{out}");
        assert!(out.contains("not for `init`"), "{out}");
    }
}

/// Every device on one I2C bus as the RP/nRF ASYNC runtimes name its const in
/// `main.rs`: `(stem, address, name as typed)`, EMPTY below two devices.
///
/// Those runtimes build their buses inline and have no `pins/configs/`, so
/// the const is all a device gets - `I2C0_DEVICE_ADDRESS` for a bus's one
/// device, `I2C0_OLED_DEVICE_ADDRESS` and so on for several. The names are
/// the ones the per-device FILES carried before a bus became a folder, and
/// they stay: the user's code below the markers names them, and a position in
/// the name would move every time an earlier device is removed.
///
/// Also the old file names of those per-device files (`<bus>_<slug>.rs`),
/// which is how a project from before the folders is read.
///
/// An unnamed device falls back to its position, and a name that collides with
/// an earlier one gets a number, so no two consts share a name.
pub fn legacy_i2c_device_stems(bus_stem: &str, cfg: &I2cModuleConfig) -> Vec<(String, u8, String)> {
    if cfg.devices.len() < 2 {
        return Vec::new();
    }
    let mut out: Vec<(String, u8, String)> = Vec::new();
    for (i, d) in cfg.devices.iter().enumerate() {
        let mut slug = sanitize_label(&d.name);
        if slug.is_empty() {
            slug = format!("device{}", i + 1);
        }
        let mut stem = format!("{bus_stem}_{slug}");
        let mut n = 2;
        while out.iter().any(|(s, _, _)| *s == stem) {
            stem = format!("{bus_stem}_{slug}_{n}");
            n += 1;
        }
        let shown = if d.name.trim().is_empty() {
            format!("device {}", i + 1)
        } else {
            d.name.clone()
        };
        out.push((stem, d.address, shown));
    }
    out
}

/// The markers of a `pins/configs/` file - not main.rs's [`GEN_BEGIN`].
const CFG_GEN_BEGIN: &str = "// <<< GENERATED>>>";
const CFG_GEN_END: &str = "// <<< GENERATED END >>>";

/// How a device file names the device it belongs to, inside its generated
/// block: `// device-id: i2c1/7`. The bus and the device's hidden uid.
pub const DEVICE_ID_TAG: &str = "// device-id:";

/// One device of an I2C bus, as its file under `pins/configs/<bus>/` is named
/// and filled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct I2cDeviceFile {
    /// 1-based position on the bus: the `#n` on the device's box.
    pub k: usize,
    /// `device{k}`, or `device{k}_{slug}` once the device has a name.
    pub stem: String,
    /// The name as typed, or `device {k}` when it has none.
    pub shown: String,
    pub address: u8,
    /// The device's hidden uid; `0` while it has none (a bus nobody has
    /// edited since it was loaded).
    pub uid: u32,
}

/// Every device on `cfg`'s bus, in the order the canvas numbers them - the
/// legacy single address included, as device 1.
///
/// The number is in the name because it is the one thing two devices never
/// share: names repeat or are empty, and every new device starts at 0x00.
pub fn i2c_device_files_of(cfg: &I2cModuleConfig) -> Vec<I2cDeviceFile> {
    cfg.rows()
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let k = i + 1;
            let slug = sanitize_label(r.name);
            let stem = if slug.is_empty() {
                format!("device{k}")
            } else {
                format!("device{k}_{slug}")
            };
            let shown = if r.name.trim().is_empty() {
                format!("device {k}")
            } else {
                r.name.replace(['\n', '\r'], " ")
            };
            let uid = match r.key {
                super::super::modules::I2cDeviceKey::Uid(u) => u,
                _ => 0,
            };
            I2cDeviceFile {
                k,
                stem,
                shown,
                address: r.address,
                uid,
            }
        })
        .collect()
}

/// What an I2C bus's `mod.rs` holds about its devices, for the generated
/// block: one `pub mod` per device file beside it - or, with no device yet, a
/// line saying where they will go.
///
/// Always ends in a newline: one template puts the END marker straight after
/// it.
pub fn i2c_device_mods(cfg: Option<&I2cModuleConfig>) -> String {
    let devices = cfg.map(i2c_device_files_of).unwrap_or_default();
    if devices.is_empty() {
        return "// No device on this bus yet - each one you add gets its own file here,\n\
                // holding its DEVICE_ADDRESS.\n"
            .to_owned();
    }
    let mut s =
        String::from("// One file per device on this bus, each holding its DEVICE_ADDRESS.\n");
    for d in devices {
        s.push_str(&format!("pub mod {};\n", d.stem));
    }
    s
}

/// An I2C bus's config files: `<bus>/mod.rs` (the bus, `bus_body`) and one
/// `<bus>/device<k>[_<name>].rs` per device - also for a bus's only device.
///
/// ONE call per backend: there are five of them, and a five-way copy of
/// "which devices, what name, what body" is five chances for one to drift.
/// `bus_stem` is the bus's module name, `i2c1` - or `twim0` on Nordic, which
/// calls the peripheral a TWIM everywhere else in its generated code.
///
/// A bus is a folder even with no device on it: switching between `i2c1.rs`
/// and `i2c1/mod.rs` on the first add and the last remove would move the
/// user's `init` twice, through a moment where both exist and neither builds.
pub fn i2c_bus_files(
    bus_stem: &str,
    bus_body: String,
    cfg: Option<&I2cModuleConfig>,
) -> Vec<(String, String)> {
    let mut out = vec![(format!("{bus_stem}/mod.rs"), bus_body)];
    for d in cfg.map(i2c_device_files_of).unwrap_or_default() {
        out.push((
            format!("{bus_stem}/{}.rs", d.stem),
            i2c_device_file(bus_stem, &d),
        ));
    }
    out
}

/// One device's config file: its address, and a place to put the code that
/// talks to it.
///
/// The BUS is not built here and cannot be. An I2C peripheral produces exactly
/// one driver, `main.rs` owns it, and every master on every HAL in this project
/// takes the address per transaction - so a device is an address plus whatever
/// the user writes around it, never a second driver. That is the whole reason
/// several devices on one pair of pads is expressible at all.
///
/// Everything that names THIS device - its number, its name, its id - is in the
/// generated block. The half below it is the same text in every device file:
/// it is kept verbatim when the file is renamed, so a name in it would be the
/// old one forever.
pub fn i2c_device_file(bus_stem: &str, d: &I2cDeviceFile) -> String {
    let mut s = String::new();
    s.push_str(CFG_GEN_BEGIN);
    s.push('\n');
    s.push_str("// Device config (from the Virtual Module) — auto-updated; edit in the module.\n");
    s.push_str(&format!(
        "// Device #{} on {}: {}\n",
        d.k,
        bus_stem.to_ascii_uppercase(),
        d.shown
    ));
    if d.uid != 0 {
        s.push_str(&format!("{DEVICE_ID_TAG} {bus_stem}/{}\n", d.uid));
    }
    s.push_str(&device_address_const(None, d.address));
    s.push_str(CFG_GEN_END);
    s.push_str("\n\n");
    s.push_str(DEVICE_FILE_TAIL);
    s.push_str(DEVICE_MOVE_NOTE);
    s
}

/// The editable half of every device file, up to [`DEVICE_MOVE_NOTE`] - see
/// [`i2c_device_file`].
const DEVICE_FILE_TAIL: &str = "\
// Everything below is editable — your changes are preserved on regeneration.
//
// This file is ONE device on the bus that `mod.rs` beside it builds. The bus
// driver exists once and `main.rs` owns its handle; this file only says which
// address on it is yours. Write the device's own routines here and take the
// bus as an argument:
//
//     pub fn read_id<I: embedded_hal::i2c::I2c>(bus: &mut I) -> Option<u8> {
//         let mut rx = [0u8; 1];
//         bus.write_read(DEVICE_ADDRESS, &[0x00], &mut rx).ok()?;
//         Some(rx[0])
//     }
//
// That is the blocking embedded-hal 1.0 shape. An async or a Native bus takes
// the same address; the example at the end of `mod.rs` shows its calls.
//
";

/// How a device file ends: what happens to it on a rename. Also what replaces
/// [`LEGACY_DEVICE_WARNING`] in a file from before buses were folders.
pub const DEVICE_MOVE_NOTE: &str = "\
// Renaming this device, or removing one listed above it, renames this file,
// and what you wrote below the markers moves with it. Code elsewhere that
// names this module by its path has to follow the new name.
";

/// The warning an old per-device file carried, which a migrated one must not:
/// its code now moves with a rename.
pub const LEGACY_DEVICE_WARNING: &str = "\
// Renaming this device in the panel renames this file, and the old one is
// removed with whatever was below its markers. Move anything you want to
// keep before you rename.
";

/// `device3_imu.rs` → `(3, "imu")`, `device2.rs` → `(2, "")`; any other file
/// name `None`.
pub fn parse_device_file_name(file: &str) -> Option<(usize, &str)> {
    let rest = file.strip_suffix(".rs")?.strip_prefix("device")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let k: usize = rest[..digits].parse().ok()?;
    let slug = match &rest[digits..] {
        "" => "",
        tail => tail.strip_prefix('_').filter(|t| !t.is_empty())?,
    };
    Some((k, slug))
}

/// The inside of a config file's FIRST generated block - the rule
/// `sync_config_files` splices by.
fn first_cfg_gen_block(body: &str) -> Option<&str> {
    let begin = body.find(CFG_GEN_BEGIN)? + CFG_GEN_BEGIN.len();
    let end = begin + body[begin..].find(CFG_GEN_END)?;
    Some(&body[begin..end])
}

/// The address a device file's generated block carries.
pub fn device_file_address(body: &str) -> Option<u8> {
    first_cfg_gen_block(body)?.lines().find_map(|l| {
        let hex = l
            .trim()
            .strip_prefix("pub const DEVICE_ADDRESS: u8 = 0x")?
            .strip_suffix(';')?;
        u8::from_str_radix(hex, 16).ok()
    })
}

/// The `(bus, uid)` a device file's generated block names, if it has one.
pub fn device_file_id(body: &str) -> Option<(&str, u32)> {
    first_cfg_gen_block(body)?.lines().find_map(|l| {
        let (bus, uid) = l
            .trim()
            .strip_prefix(DEVICE_ID_TAG)?
            .trim()
            .rsplit_once('/')?;
        Some((bus, uid.parse().ok()?))
    })
}

/// The names the devices `(k, slug)` - in order - would have had as flat files
/// before buses were folders: `<bus>_<slug>`, `<bus>_device<k>` for an unnamed
/// one, and `_2`, `_3` on a repeat. The same rule as
/// [`legacy_i2c_device_stems`], from a device list read back off file names.
///
/// What lets a project from before the folders find each old file's device by
/// its NAME even when nothing else tells them apart - two unnamed devices
/// still at 0x00, say.
pub fn legacy_device_stems_for(bus_stem: &str, devices: &[(usize, &str)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (k, slug) in devices {
        let base = if slug.is_empty() {
            format!("device{k}")
        } else {
            (*slug).to_owned()
        };
        let mut stem = format!("{bus_stem}_{base}");
        let mut n = 2;
        while out.contains(&stem) {
            stem = format!("{bus_stem}_{base}_{n}");
            n += 1;
        }
        out.push(stem);
    }
    out
}

// ── Edge hooks — the user's handler for an armed input ───────────────────────
//
// The task (or interrupt handler) an armed input generates sits INSIDE the
// markers, so a body typed into it was lost on the next regeneration - any
// change on the Pins, Peripherals or Clock tab. The generated code calls a
// function instead: seeded ONCE below the user tail, and never rewritten.

/// One armed input's hook, reported by the backend that emitted the call
/// (`FamilyBackend::edge_hooks`) so `Mcu` can seed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeHook {
    /// The function the generated handler calls: [`edge_hook_name`].
    pub name: String,
    /// The pin, as the generated comment names it (`P0.14 (pad 5, BTN_A)`).
    pub what: String,
    /// The generated task or handler that calls it, for the seed's doc.
    pub caller: String,
    /// `async fn` where the caller is an executor task, so the body can
    /// `.await` a debounce; plain `fn` where it runs in an interrupt handler.
    pub is_async: bool,
}

/// The hook's name, from the binding the handler is named after:
/// `p0_14_in` → `on_p0_14_in_edge`.
pub fn edge_hook_name(binding: &str) -> String {
    format!("on_{binding}_edge")
}

/// The seed: the hook as it is first written below the tail. Names no crate
/// on purpose - it has to compile under either runtime, since a switch to a
/// runtime with no handler leaves it as dead code rather than removing it.
/// The edge is left out of the doc: changing it does not rename the hook, and
/// a stale line in the user's text is not ours to fix.
///
/// The async seed has no `.await`, and `clippy::unused_async` is in
/// `pedantic`, which the Strict profile denies - without the `#[allow]` a
/// file the IDE just wrote would fail `cargo clippy`.
pub fn hook_seed(hook: &EdgeHook) -> String {
    if hook.is_async {
        format!(
            "/// {} - called from the `{}` task\n\
             /// with the level after the edge. Yours: kept across regeneration.\n\
             #[allow(clippy::unused_async)] // drop once the body awaits something\n\
             async fn {}(_high: bool) {{\n\
             \x20   // Your code here.\n\
             }}\n",
            hook.what, hook.caller, hook.name
        )
    } else {
        format!(
            "/// {} - called from `{}` with the level after the edge. Runs in the\n\
             /// interrupt, inside a critical section: keep it short.\n\
             /// Yours: kept across regeneration.\n\
             fn {}(_high: bool) {{\n\
             \x20   // Your code here.\n\
             }}\n",
            hook.what, hook.caller, hook.name
        )
    }
}

/// Seed each hook the user's region does not already have. The precedent is
/// [`ensure_module_models`]: append at the end of the file when absent, and
/// otherwise leave the file alone - the hook is the user's the moment it is
/// written, and a body they typed is never touched.
///
/// "Has" is the bare identifier anywhere AFTER `GEN_END`, not `fn name(` over
/// the whole file: a user who moves the hook into `src/handlers.rs` and writes
/// `use handlers::on_p0_14_in_edge;` in the tail has no definition left here,
/// and seeding one would collide with the `use`. The block cannot give a false
/// hit, since it sits before `GEN_END` and holds the call, never a definition.
pub fn ensure_edge_hooks(mut file: String, hooks: &[EdgeHook]) -> String {
    let user_region_starts = file.find(GEN_END).map_or(0, |i| i + GEN_END.len());
    let seeds: Vec<String> = hooks
        .iter()
        .filter(|h| !has_ident(&file[user_region_starts..], &h.name))
        .map(hook_seed)
        .collect();
    if seeds.is_empty() {
        return file;
    }
    if !file.ends_with('\n') {
        file.push('\n');
    }
    for seed in seeds {
        file.push('\n');
        file.push_str(&seed);
    }
    file
}

/// Whether `ident` occurs in `text` as a whole identifier - not as the tail of
/// `on_p0_14_in_edge_old`, nor the head of `on_p0_14_in_edge2`.
fn has_ident(text: &str, ident: &str) -> bool {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(ident).any(|(at, _)| {
        let before = text[..at].chars().next_back().is_none_or(|c| !is_ident(c));
        let after = text[at + ident.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_ident(c));
        before && after
    })
}

// ── Variable name suffix ──────────────────────────────────────────────────────

use super::super::pins::logic::pin_function::PinFunction;

/// The `<type>` half of a generated binding name `<pin>_<type>`, e.g.
/// `out` / `in` / `i2c1_sda` / `spi2_sck` / `usart1_tx` / `adc1_in0`. So a
/// PC13 output binds as `pc13_out`, a PB9 I2C1 SDA as `pb9_i2c1_sda`.
/// The sub-block letter inside a generated SAI variable name.
fn sai_tag(block: u8) -> &'static str {
    if block == 1 { "a" } else { "b" }
}

pub fn var_suffix(func: &PinFunction) -> String {
    match func {
        PinFunction::GpioOutput => "out".into(),
        PinFunction::GpioInput => "in".into(),
        PinFunction::GpioAnalog => "analog".into(),
        PinFunction::AdcChannel { adc, channel } => format!("adc{adc}_in{channel}"),
        PinFunction::TimerPwm { timer, channel } => format!("tim{timer}_ch{channel}"),
        PinFunction::TimerPwmN { timer, channel } => format!("tim{timer}_ch{channel}n"),
        PinFunction::TimerBreak { timer, input } => format!("tim{timer}_bkin{input}"),
        PinFunction::UsartTx(n) => format!("usart{n}_tx"),
        PinFunction::UsartRx(n) => format!("usart{n}_rx"),
        PinFunction::UsartCts(n) => format!("usart{n}_cts"),
        PinFunction::UsartRts(n) => format!("usart{n}_rts"),
        PinFunction::UsartCk(n) => format!("usart{n}_ck"),
        PinFunction::LpuartTx(n) => format!("lpuart{n}_tx"),
        PinFunction::LpuartRx(n) => format!("lpuart{n}_rx"),
        PinFunction::LpuartCts(n) => format!("lpuart{n}_cts"),
        PinFunction::LpuartRts(n) => format!("lpuart{n}_rts"),
        PinFunction::SpiSck(n) => format!("spi{n}_sck"),
        PinFunction::SpiMosi(n) => format!("spi{n}_mosi"),
        PinFunction::SpiMiso(n) => format!("spi{n}_miso"),
        PinFunction::SpiNss(n) => format!("spi{n}_nss"),
        PinFunction::SpiRdy(n) => format!("spi{n}_rdy"),
        PinFunction::DacOut { dac, channel } => format!("dac{dac}_out{channel}"),
        PinFunction::HspiClk { unit } => format!("hspi{unit}_clk"),
        PinFunction::HspiNcs { unit } => format!("hspi{unit}_ncs"),
        PinFunction::HspiDqs { unit, index } => format!("hspi{unit}_dqs{index}"),
        PinFunction::HspiIo { unit, lane } => format!("hspi{unit}_io{lane}"),
        PinFunction::XspiClk { port } => format!("xspi_p{port}_clk"),
        PinFunction::XspiNcs { port, cs } => format!("xspi_p{port}_ncs{cs}"),
        PinFunction::XspiDqs { port, index } => format!("xspi_p{port}_dqs{index}"),
        PinFunction::XspiIo { port, lane } => format!("xspi_p{port}_io{lane}"),
        PinFunction::OspiClk { port } => format!("ospi_p{port}_clk"),
        PinFunction::OspiNcs { port } => format!("ospi_p{port}_ncs"),
        PinFunction::OspiDqs { port } => format!("ospi_p{port}_dqs"),
        PinFunction::OspiIo { port, lane } => format!("ospi_p{port}_io{lane}"),
        PinFunction::QspiClk => "qspi_clk".into(),
        PinFunction::QspiNcs { bank } => format!("qspi_b{bank}_ncs"),
        PinFunction::QspiIo { bank, lane } => format!("qspi_b{bank}_io{lane}"),
        PinFunction::SdmmcCk { unit } => format!("sdmmc{unit}_ck"),
        PinFunction::SdmmcCmd { unit } => format!("sdmmc{unit}_cmd"),
        PinFunction::SdmmcD { unit, lane } => format!("sdmmc{unit}_d{lane}"),
        PinFunction::SaiSck { sai, block } => format!("sai{sai}{}_sck", sai_tag(*block)),
        PinFunction::SaiSd { sai, block } => format!("sai{sai}{}_sd", sai_tag(*block)),
        PinFunction::SaiFs { sai, block } => format!("sai{sai}{}_fs", sai_tag(*block)),
        PinFunction::SaiMclk { sai, block } => format!("sai{sai}{}_mclk", sai_tag(*block)),
        PinFunction::RmtChannel(n) => format!("rmt{n}"),
        PinFunction::TouchPad(n) => format!("touch{n}"),
        PinFunction::LcdCamData { lane } => format!("lcd_d{lane}"),
        PinFunction::LcdCamDc => "lcd_dc".to_owned(),
        PinFunction::LcdCamWr => "lcd_wr".to_owned(),
        PinFunction::LcdCamCs => "lcd_cs".to_owned(),
        PinFunction::LcdCamPclk => "lcd_pclk".to_owned(),
        PinFunction::LcdCamVsync => "lcd_vsync".to_owned(),
        PinFunction::LcdCamHsync => "lcd_hsync".to_owned(),
        PinFunction::LcdCamDe => "lcd_de".to_owned(),
        PinFunction::CamData { lane } => format!("cam_d{lane}"),
        PinFunction::CamPclk => "cam_pclk".to_owned(),
        PinFunction::CamVsync => "cam_vsync".to_owned(),
        PinFunction::CamHsync => "cam_hsync".to_owned(),
        PinFunction::CamHenable => "cam_href".to_owned(),
        PinFunction::CamMclk => "cam_mclk".to_owned(),
        PinFunction::ParlData { lane } => format!("parl_d{lane}"),
        PinFunction::ParlClk => "parl_clk".to_owned(),
        PinFunction::ParlValid => "parl_valid".to_owned(),
        PinFunction::ParlRxData { lane } => format!("parl_rx_d{lane}"),
        PinFunction::ParlRxClk => "parl_rx_clk".to_owned(),
        PinFunction::ParlRxValid => "parl_rx_valid".to_owned(),
        PinFunction::McpwmA { unit, operator } => format!("mcpwm{unit}_op{operator}a"),
        PinFunction::McpwmB { unit, operator } => format!("mcpwm{unit}_op{operator}b"),
        PinFunction::PcntEdge { unit, channel } => format!("pcnt{unit}_edge{channel}"),
        PinFunction::PcntCtrl { unit, channel } => format!("pcnt{unit}_ctrl{channel}"),
        PinFunction::I2sCk(n) => format!("i2s{n}_ck"),
        PinFunction::I2sWs(n) => format!("i2s{n}_ws"),
        PinFunction::I2sSd(n) => format!("i2s{n}_sd"),
        PinFunction::I2sMck(n) => format!("i2s{n}_mck"),
        PinFunction::I2cScl(n) => format!("i2c{n}_scl"),
        PinFunction::I2cSda(n) => format!("i2c{n}_sda"),
        PinFunction::UsbDm => "usb_dm".into(),
        PinFunction::UsbDp => "usb_dp".into(),
        PinFunction::CanRx => "can_rx".into(),
        PinFunction::CanTx => "can_tx".into(),
        PinFunction::SwdIo => "swd_io".into(),
        PinFunction::SwdClk => "swd_clk".into(),
        PinFunction::Mco => "mco".into(),
        // Generic AF: the signal name lowercased is already a valid Rust
        // identifier fragment (`SAI1_SD_A` → `sai1_sd_a`); `-` (as in
        // `JTDO-TRACESWO`) becomes `_`.
        PinFunction::Other(name) => name.to_ascii_lowercase().replace('-', "_"),
        PinFunction::Unset => "unset".into(),
    }
}

// ── Custom label → binding name ───────────────────────────────────────────────

/// Sanitize a user-typed pin label into a Rust-identifier fragment: lowercase
/// ASCII alphanumerics kept, every other run collapsed to a single `_`, with
/// leading/trailing `_` trimmed. Returns "" when nothing usable remains.
///
/// e.g. `"Status LED"` → `status_led`, `"  D7! "` → `d7`.
/// A hundredths-of-a-percent duty as a plain percentage: `750` -> `"7.5"`,
/// `7525` -> `"75.25"`, `10_000` -> `"100"`.
///
/// Trailing zeros are trimmed, because a generated comment reading "75.00 %"
/// only adds noise to the common case.
pub fn duty_percent_str(x100: u16) -> String {
    let whole = x100 / 100;
    match x100 % 100 {
        0 => format!("{whole}"),
        r if r % 10 == 0 => format!("{whole}.{}", r / 10),
        r => format!("{whole}.{r:02}"),
    }
}

pub fn sanitize_label(label: &str) -> String {
    let mut out = String::new();
    let mut pending_sep = false;
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_sep && !out.is_empty() {
                out.push('_');
            }
            pending_sep = false;
            out.push(ch.to_ascii_lowercase());
        } else {
            pending_sep = true;
        }
    }
    out
}

/// Full generated binding name: `<base>_<type>` with the user's sanitized label
/// appended as `_<label>` when present. So a `pc13` output (base `pc13`) labelled
/// "led" binds as `pc13_out_led`; with no label it stays `pc13_out`.
pub fn pin_binding(base_var: &str, func: &PinFunction, custom_label: &str) -> String {
    let mut s = format!("{}_{}", base_var, var_suffix(func));
    let extra = sanitize_label(custom_label);
    if !extra.is_empty() {
        s.push('_');
        s.push_str(&extra);
    }
    s
}

/// One per-pin `let` binding found inside a generated `main.rs` GEN block.
pub struct GenBinding<'a> {
    /// 1-based line number in the FULL source (not just the block).
    pub line: usize,
    /// The binding variable, e.g. `pc13_out_led`.
    pub var: &'a str,
    /// MCU pin the binding belongs to, e.g. `PC13` / `GPIO20`.
    pub pin_name: String,
    /// The trimmed source line.
    pub text: &'a str,
}

/// Scan the GEN block of a generated `main.rs` for its per-pin `let` bindings.
///
/// Covers both shapes the backends emit — `let [mut] pXY… = …` (STM32 blocking
/// and embassy) and `let [mut] gpioNN… = …` (ESP) — and, importantly, `let mut`
/// as well as plain `let`: embassy and ESP bind every OUTPUT as `let mut`.
///
/// This is the single place that knows what a generated binding line looks like;
/// [`parse_pin_labels`] and [`find_pin_binding_line`] both read through it, so a
/// lookup can't drift away from the generator.
pub fn gen_let_bindings(source: &str) -> Vec<GenBinding<'_>> {
    let (Some(begin_pos), Some(end_pos)) = (source.find(GEN_BEGIN), source.find(GEN_END)) else {
        return vec![];
    };
    if begin_pos >= end_pos {
        return vec![];
    }
    // Lines fully before the block — the offset that turns a block-local index
    // into an absolute 1-based line number.
    let base_line = source[..begin_pos].lines().count();

    let mut out = Vec::new();
    for (i, line) in source[begin_pos..end_pos].lines().enumerate() {
        let trimmed = line.trim();
        let after_let = match trimmed.strip_prefix("let mut ") {
            Some(r) => r,
            None => match trimmed.strip_prefix("let ") {
                Some(r) => r,
                None => continue,
            },
        };
        let Some(eq_pos) = after_let.find(" =") else {
            continue;
        };
        let var = after_let[..eq_pos].trim();

        // `pXY…` → port letter + number; `gpioNN…` → number.
        let pin_name = if let Some(rest) = var.strip_prefix("gpio") {
            let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if num.is_empty() {
                continue;
            }
            format!("GPIO{num}")
        } else if var.len() >= 3 && var.starts_with('p') {
            let port_lc = match var.chars().nth(1) {
                Some(c) if c.is_ascii_lowercase() => c,
                _ => continue,
            };
            let num: String = var[2..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if num.is_empty() {
                continue;
            }
            format!("P{}{}", port_lc.to_ascii_uppercase(), num)
        } else {
            continue;
        };

        out.push(GenBinding {
            line: base_line + i + 1,
            var,
            pin_name,
            text: trimmed,
        });
    }
    out
}

/// Line (1-based) + binding name of a pin's `let` in the generated GEN block —
/// what "jump to where this pin is defined" lands on. `None` when the pin has no
/// binding of its own (unconfigured, or consumed inline by a peripheral).
pub fn find_pin_binding_line(source: &str, pin_name: &str) -> Option<(usize, String)> {
    gen_let_bindings(source)
        .into_iter()
        .find(|b| b.pin_name.eq_ignore_ascii_case(pin_name))
        .map(|b| (b.line, b.var.to_owned()))
}

/// Fallback for a pin with no `let` of its own: the first GEN-block line that
/// mentions it as a whole token — `p.PB6`, `peripherals.GPIO20`, `gpiob.pb6`.
/// ESP hands bus pins straight to their driver (`.with_rx(peripherals.GPIO20)`),
/// so that call IS the definition site.
pub fn find_pin_mention_line(source: &str, pin_name: &str) -> Option<usize> {
    let (Some(begin_pos), Some(end_pos)) = (source.find(GEN_BEGIN), source.find(GEN_END)) else {
        return None;
    };
    if begin_pos >= end_pos {
        return None;
    }
    let base_line = source[..begin_pos].lines().count();
    let upper = pin_name.to_ascii_uppercase();
    let lower = pin_name.to_ascii_lowercase();
    // Whole-token match, so `GPIO2` never lights up on `GPIO20` and the bare
    // `pb6` never matches the binding `pb6_out` (that one is case 1's job).
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let hit = |line: &str, needle: &str| {
        let mut from = 0;
        while let Some(rel) = line[from..].find(needle) {
            let s = from + rel;
            let e = s + needle.len();
            let before_ok = s == 0 || !line[..s].ends_with(is_word);
            let after_ok = e >= line.len() || !line[e..].starts_with(is_word);
            if before_ok && after_ok {
                return true;
            }
            from = s + 1;
        }
        false
    };
    source[begin_pos..end_pos]
        .lines()
        .enumerate()
        .find(|(_, l)| hit(l, &upper) || hit(l, &lower))
        .map(|(i, _)| base_line + i + 1)
}

/// Recover the user labels embedded in generated binding names, mirroring
/// [`parse_main_rs`]. Scans the GEN block's per-pin `let` bindings and, for each
/// one carrying a `<base>_<type>_<label>` suffix, returns `(pin_name, label)`.
/// Only GPIO/PWM bindings (the ones the rename field targets) can carry a label.
pub fn parse_pin_labels(source: &str) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for b in gen_let_bindings(source) {
        let (var, pin_name, trimmed) = (b.var, b.pin_name, b.text);

        // Function from the comment, then strip the `<base>_<type>` prefix; the
        // remaining `_<label>` (if any) is the user's custom name.
        let Some(comment_pos) = trimmed.rfind("// ") else {
            continue;
        };
        let label_str = trimmed[comment_pos + 3..]
            .trim()
            .trim_end_matches(';')
            .trim();
        let Some(func) = PinFunction::from_label(label_str) else {
            continue;
        };

        let needle = format!("_{}", var_suffix(&func));
        if let Some(pos) = var.find(&needle) {
            let after_suffix = &var[pos + needle.len()..];
            if let Some(label) = after_suffix.strip_prefix('_') {
                if !label.is_empty() {
                    result.push((pin_name, label.to_owned()));
                }
            }
        }
    }
    result
}

// ── Pin state parser ──────────────────────────────────────────────────────────

/// Parses pin assignments from an existing `src/main.rs`.
///
/// Scans the GEN_BEGIN … GEN_END block for lines of the form:
/// ```text
///     let p{lc}{num} = [&mut ]{pv}.p{lc}{num}.{method}(…); // {label}
/// ```
/// Returns `(pin_name, PinFunction)` pairs (e.g. `("PC13", GpioOutput)`)
/// for every recognisable pin.  Unknown or comment-only lines are skipped.
///
/// Handles both STM32 format ("let pc13 = …") and ESP32 format ("let gpio2 = …").
///
/// The label may also sit on its OWN line, immediately above the code it
/// describes — the shape the ESP bus builders emit:
/// ```text
///     // USART0  RX
///     .with_rx(peripherals.GPIO20)
/// ```
/// They moved there because a trailing comment SWALLOWS the chain's terminating
/// `;`. Without this, every USART/SPI/I2C pin on an ESP project was lost on
/// reload: `apply_saved_pins` clears the diagram and re-applies only what parsed
/// here, so an unparsed pin comes back Unset and its Virtual Module unwired.
pub fn parse_main_rs(source: &str) -> Vec<(String, PinFunction)> {
    let Some(begin_pos) = source.find(GEN_BEGIN) else {
        return vec![];
    };
    let Some(end_pos) = source.find(GEN_END) else {
        return vec![];
    };
    if begin_pos >= end_pos {
        return vec![];
    }

    let gen_block = &source[begin_pos..end_pos];
    let mut result = Vec::new();
    // A comment-only line labels the line RIGHT below it (see the doc comment).
    // Deliberately one line of memory, not a running "last comment seen": a
    // section header like `// ── UART0 ──` must not leak onto a `.with_` line
    // three lines further down. Consumed (taken) by whatever line follows it,
    // matched or not.
    let mut pending_label: Option<String> = None;

    for line in gen_block.lines() {
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("//") {
            pending_label = Some(rest.trim().to_owned());
            continue;
        }
        let label_above = pending_label.take();

        // ── STM32: "let [mut ]p{port}{num} = ..." ────────────────────────────
        // trimmed = "let pc13 = &mut gpioc.pc13.into_push_pull_output(…); // …"
        // The `mut` form is emitted by the WBA (embassy) backend for outputs
        // (`let mut pb5 = Output::new(…)`) — strip it so both shapes parse.
        if trimmed.starts_with("let p") || trimmed.starts_with("let mut p") {
            let after_let = trimmed
                .strip_prefix("let mut ")
                .or_else(|| trimmed.strip_prefix("let "))
                .unwrap_or(trimmed); // "pc13 = …"
            let Some(eq_pos) = after_let.find(" =") else {
                continue;
            };
            let var = after_let[..eq_pos].trim(); // "pc13"

            // var must be p + ascii-lowercase-letter + one-or-more digits
            if var.len() < 3 || !var.starts_with('p') {
                continue;
            }
            let port_lc = match var.chars().nth(1) {
                Some(c) if c.is_ascii_lowercase() => c,
                _ => continue,
            };
            // Read the pin-number digits, stopping at the `_<type>` suffix (so
            // both `pc13` and `pc13_out` yield "13").
            let pin_num_str: String = var[2..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if pin_num_str.is_empty() {
                continue;
            }

            // "pc13" / "pc13_out" → "PC13"
            let pin_name = format!("P{}{}", port_lc.to_ascii_uppercase(), pin_num_str);

            let Some(comment_pos) = trimmed.rfind("// ") else {
                continue;
            };
            let label = trimmed[comment_pos + 3..].trim();

            if let Some(func) = PinFunction::from_label(label) {
                result.push((pin_name, func));
            }
            continue;
        }

        // ── ESP32 GPIO / ADC per-pin bindings ────────────────────────────────
        //   let mut gpio2 = Output::new(peripherals.GPIO2, Level::Low); // GPIO Output
        //   let gpio9 = Input::new(peripherals.GPIO9, InputConfig::default().with_pull(Pull::None)); // GPIO Input
        //   let mut gpio0_adc = adc1_config                              // ADC1  IN0
        //       .enable_pin(peripherals.GPIO0, Attenuation::_11dB);
        //
        // STM32 port-split lines ("let mut gpioa = dp.GPIOA.split()") are also
        // caught by these guards, but they fail the "starts with digit" check below.
        if trimmed.starts_with("let mut gpio") || trimmed.starts_with("let gpio") {
            let after_let = if trimmed.starts_with("let mut ") {
                &trimmed["let mut ".len()..]
            } else {
                &trimmed["let ".len()..]
            };
            let Some(eq_pos) = after_let.find(" =") else {
                continue;
            };
            let var = after_let[..eq_pos].trim(); // "gpio2", "gpio9", "gpio0_adc"

            // Must be "gpio" + digit  →  filters out "gpioa"/"gpiob" port splits
            let gpio_rest = match var.strip_prefix("gpio") {
                Some(r) if r.starts_with(|c: char| c.is_ascii_digit()) => r,
                _ => continue,
            };

            // Read the pin-number digits, stopping at any `_<type>` suffix (so
            // `gpio2`, `gpio2_out`, `gpio0_adc1_in0` all yield the number).
            let pin_num_str: String = gpio_rest
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if pin_num_str.is_empty() {
                continue;
            }

            let pin_name = format!("GPIO{pin_num_str}"); // "GPIO2", "GPIO0"

            let Some(comment_pos) = trimmed.rfind("// ") else {
                continue;
            };
            // The function comes from the comment label (robust to the binding
            // name format). Strip a trailing ';' from single-method init lines.
            let label = trimmed[comment_pos + 3..]
                .trim()
                .trim_end_matches(';')
                .trim();
            if let Some(func) = PinFunction::from_label(label) {
                result.push((pin_name, func));
            }
            continue;
        }

        // ── ESP32 peripheral chain lines ──────────────────────────────────────
        //   .with_rx(peripherals.GPIO20)  // USART0  RX
        //   .with_tx(peripherals.GPIO21)  // USART0  TX;   ← ';' on last method
        //   .with_sck(peripherals.GPIO6)  // SPI2  SCK
        //   .with_scl(peripherals.GPIO10) // I2C0  SCL
        if trimmed.starts_with(".with_") {
            // Extract GPIO number from "peripherals.GPIO{N}"
            let Some(gpio_pos) = trimmed.find("peripherals.GPIO") else {
                continue;
            };
            let after_gpio = &trimmed[gpio_pos + "peripherals.GPIO".len()..];
            let num_str: String = after_gpio
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if num_str.is_empty() {
                continue;
            }
            let pin_name = format!("GPIO{num_str}");

            // Either shape: the label trailing the call (older projects, still
            // on disk) or sitting on the line above it (what is generated now).
            let trailing = trimmed
                .rfind("// ")
                .map(|p| trimmed[p + 3..].trim().trim_end_matches(';').trim());
            let Some(label) = trailing.or(label_above.as_deref()) else {
                continue;
            };

            if let Some(func) = PinFunction::from_label(label) {
                result.push((pin_name, func));
            }
            continue;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    /// The comment beside a generated duty reads as a person would write it.
    #[test]
    fn a_duty_reads_as_a_plain_percentage() {
        for (x100, want) in [
            (0u16, "0"),
            (750, "7.5"),  // the servo case whole percent could not express
            (7_500, "75"), // no trailing ".00" on the common case
            (7_550, "75.5"),
            (7_505, "75.05"),
            (10_000, "100"),
        ] {
            assert_eq!(duty_percent_str(x100), want, "{x100} hundredths");
        }
    }

    use super::*;

    /// Under strict lints, EVERY chip on EVERY runtime gets its generated entry
    /// exempted.
    ///
    /// The list of entry attributes is hand-written and the match is exact, so a
    /// backend that emits a new one silently loses the exemption - which is what
    /// happened to ESP Async: it emits `#[esp_rtos::main]` while Blocking emits
    /// `#[esp_hal::main]`, only the latter was listed, and the generated init was
    /// then linted in full inside a block the user cannot edit.
    ///
    /// Derived from the real `fresh_main_rs` of each definition, and the entry
    /// is found by SHAPE - the attribute directly above `fn main` - not by a
    /// second copy of the attribute list. It was a second copy once, in spite
    /// of the sentence above saying otherwise: a chip whose attribute was on
    /// neither list had "no entry", was skipped, and passed. That is how the
    /// micro:bit's `#[cortex_m_rt::entry]` and the Pico's `#[rp2040_hal::entry]`
    /// / `#[rp235x_hal::entry]` went unexempted on Blocking with this test green.
    #[test]
    fn strict_lints_exempt_the_generated_entry_on_every_chip_and_runtime() {
        use crate::panels::mcu_module::builtins::builtin_definitions;
        use crate::panels::mcu_module::mcu::model::Runtime;

        let mut seen = std::collections::BTreeSet::new();
        for d in builtin_definitions() {
            for rt in [Runtime::Blocking, Runtime::Async, Runtime::Native] {
                let mut mcu = d.build_mcu();
                mcu.runtime = rt;
                let code = mcu.fresh_main_rs();
                // A runtime a family cannot build emits no `main` to exempt.
                let Some(attr) = entry_attribute_of(&code) else {
                    continue;
                };
                seen.insert(attr.to_owned());
                let exempt = strict_main_exemption(code.clone(), true);
                // Directly above the entry attribute, which is where it has to
                // be to land on `main` and on nothing else.
                let lines: Vec<&str> = exempt.lines().map(str::trim).collect();
                let at = lines.iter().position(|l| *l == attr);
                let above = at.and_then(|i| i.checked_sub(1)).map(|i| lines[i]);
                assert!(
                    above.is_some_and(|l| l.starts_with("#[allow(clippy::")),
                    "{} / {rt:?}: `{attr}` is not on the list, so the generated init is \
                     linted in full",
                    d.id
                );
            }
        }
        // Both directions. Every spelling on the list came out of a generator
        // just now - so the loop saw what it exists for, not a lucky subset,
        // and the list carries nothing dead. The other direction, an emitted
        // spelling that is not listed, is the assertion inside the loop.
        for listed in ENTRY_ATTRIBUTES {
            assert!(
                seen.contains(listed),
                "{listed} is listed but no built-in emits it: {seen:?}"
            );
        }
    }

    /// The entry attribute of `fn main` / `async fn main`, whatever it is
    /// called - the test's own way of finding the entry.
    ///
    /// Out of the whole run of attributes above `main`, the one that is not an
    /// `#[allow]`: the embassy STM32 Blocking template puts
    /// `#[allow(unused_variables, unused_mut)]` between `#[entry]` and the
    /// function, and "the line above" would have taken that for the entry.
    fn entry_attribute_of(code: &str) -> Option<&str> {
        let lines: Vec<&str> = code.lines().map(str::trim).collect();
        let main = lines
            .iter()
            .position(|l| l.starts_with("fn main(") || l.starts_with("async fn main("))?;
        lines[..main]
            .iter()
            .rev()
            .take_while(|l| l.starts_with("#["))
            .find(|l| !l.starts_with("#[allow("))
            .copied()
    }

    /// The helper above, on the one shape no built-in produces.
    #[test]
    fn the_entry_is_found_past_an_allow_between_it_and_main() {
        let embassy_blocking =
            "use x;\n\n#[entry]\n#[allow(unused_variables, unused_mut)]\nfn main() -> ! {\n}\n";
        assert_eq!(entry_attribute_of(embassy_blocking), Some("#[entry]"));
        // And the exemption lands above the entry there too.
        let out = strict_main_exemption(embassy_blocking.to_owned(), true);
        assert!(
            out.contains("clippy::as_conversions)]\n#[entry]\n"),
            "{out}"
        );
        // No attribute at all is "no entry", not a panic or a stray line.
        assert_eq!(entry_attribute_of("// note\nfn main() {}\n"), None);
        assert_eq!(entry_attribute_of("fn other() {}\n"), None);
    }

    /// And the ESP async attribute specifically, named so a rename is loud.
    #[test]
    fn the_esp_async_entry_is_exempted() {
        let code = "#[esp_rtos::main]\nasync fn main(_spawner: Spawner) {\n}\n".to_owned();
        let out = strict_main_exemption(code.clone(), true);
        assert!(out.starts_with("#[allow(clippy::"), "{out}");
        assert_eq!(
            strict_main_exemption(code, false),
            "#[esp_rtos::main]\nasync fn main(_spawner: Spawner) {\n}\n",
            "and still a no-op when strict is off"
        );
    }

    #[test]
    fn strict_main_exemption_wraps_entry_only_when_strict() {
        let code =
            "// GEN\nuse foo;\n#[entry]\nfn main() -> ! {\n    let dp = take().unwrap();\n}\n";
        // Off → unchanged.
        assert_eq!(strict_main_exemption(code.to_string(), false), code);
        // On → an #[allow(...)] appears immediately before #[entry].
        let on = strict_main_exemption(code.to_string(), true);
        assert!(
            on.contains("#[allow(clippy::pedantic"),
            "allow added:\n{on}"
        );
        assert!(on.contains("clippy::unwrap_used"), "lints listed:\n{on}");
        let allow_pos = on.find("#[allow(").unwrap();
        let entry_pos = on.find("#[entry]").unwrap();
        assert!(allow_pos < entry_pos, "allow precedes entry:\n{on}");
        // Only one allow (no accumulation on a second pass over fresh codegen).
        assert_eq!(on.matches("#[allow(clippy::pedantic").count(), 1);
    }

    #[test]
    fn strict_main_exemption_handles_embassy_entry() {
        let code = "#[embassy_executor::main]\nasync fn main(s: Spawner) {}\n";
        let on = strict_main_exemption(code.to_string(), true);
        assert!(on.starts_with("#[allow(clippy::"), "allow first:\n{on}");
        assert!(on.contains("#[embassy_executor::main]"));
    }

    #[test]
    fn strict_config_exemption_inserts_module_allow_after_marker() {
        let body = "// <<< GENERATED>>>\npub const BAUDRATE: u32 = 115200;\n// <<< GENERATED END >>>\n\nuse foo;\n";
        assert_eq!(strict_config_exemption(body.to_string(), false), body);
        let on = strict_config_exemption(body.to_string(), true);
        // Module inner attribute, right after the marker, before the const.
        let attr = on.find("#![allow(clippy::").unwrap();
        let marker = on.find("// <<< GENERATED>>>").unwrap();
        let konst = on.find("pub const BAUDRATE").unwrap();
        assert!(
            marker < attr && attr < konst,
            "attr between marker and const:\n{on}"
        );
    }

    #[test]
    fn mcu_id_marker_round_trips() {
        let line = mcu_id_marker_line("esp32c3-graph");
        assert_eq!(line, "// rust_on_chip:mcu=esp32c3-graph\n");
        // Embedded anywhere in a file, possibly indented, parse_mcu_id finds it.
        let src = format!("// Auto-generated\n{line}#![no_std]\n");
        assert_eq!(parse_mcu_id(&src).as_deref(), Some("esp32c3-graph"));
    }

    /// Every project created before the rename carries the old marker, and most
    /// backends never rewrite that header. Losing it reopens an imported chip as the
    /// first built-in sharing its HAL — and Save regenerates for that one.
    #[test]
    fn a_marker_written_before_the_rename_still_identifies_the_chip() {
        let src = "// Auto-generated by Embedded IDE\n  // embedded-ide:mcu=stm32f103rb\n";
        assert_eq!(parse_mcu_id(src).as_deref(), Some("stm32f103rb"));
    }

    #[test]
    fn empty_id_emits_no_marker() {
        assert_eq!(mcu_id_marker_line(""), "");
        assert!(parse_mcu_id("// Auto-generated\n#![no_std]\n").is_none());
    }

    #[test]
    fn sanitize_label_makes_identifier_fragments() {
        assert_eq!(sanitize_label("led"), "led");
        assert_eq!(sanitize_label("Status LED"), "status_led");
        assert_eq!(sanitize_label("  D7! "), "d7");
        assert_eq!(sanitize_label("a--b__c"), "a_b_c");
        assert_eq!(sanitize_label(""), "");
        assert_eq!(sanitize_label("***"), "");
    }

    #[test]
    fn pin_binding_appends_sanitized_label() {
        // No label → plain `<base>_<type>`.
        assert_eq!(
            pin_binding("pc13", &PinFunction::GpioOutput, ""),
            "pc13_out"
        );
        // Label appended and sanitized.
        assert_eq!(
            pin_binding("pc13", &PinFunction::GpioOutput, "Status LED"),
            "pc13_out_status_led"
        );
        // ESP-style base + ADC suffix.
        assert_eq!(
            pin_binding("gpio0", &PinFunction::AdcChannel { adc: 1, channel: 0 }, ""),
            "gpio0_adc1_in0"
        );
    }

    #[test]
    fn parse_pin_labels_recovers_custom_names() {
        let src = format!(
            "{GEN_BEGIN}\n\
             let pc13_out_led = &mut gpioc.pc13.into_push_pull_output(&mut gpioc.crh); // GPIO Output\n\
             let pa1_in = &mut gpioa.pa1.into_floating_input(&mut gpioa.crl); // GPIO Input\n\
             let mut gpio2_out_relay = Output::new(peripherals.GPIO2, Level::Low); // GPIO Output\n\
             {GEN_END}"
        );
        let labels = parse_pin_labels(&src);
        // Pins with a label are recovered; the unlabelled one is absent.
        assert!(labels.contains(&("PC13".to_owned(), "led".to_owned())));
        assert!(labels.contains(&("GPIO2".to_owned(), "relay".to_owned())));
        assert!(!labels.iter().any(|(n, _)| n == "PA1"));
    }

    #[test]
    fn pin_binding_round_trips_through_parse_pin_labels() {
        let var = pin_binding("pc13", &PinFunction::GpioOutput, "My Pin 7");
        assert_eq!(var, "pc13_out_my_pin_7");
        let src = format!(
            "{GEN_BEGIN}\nlet {var} = &mut gpioc.pc13.into_push_pull_output(&mut gpioc.crh); // GPIO Output\n{GEN_END}"
        );
        assert_eq!(
            parse_pin_labels(&src),
            vec![("PC13".to_owned(), "my_pin_7".to_owned())]
        );
    }

    /// The shape each backend emits, so "jump to this pin" lands on the right
    /// line no matter which one generated main.rs. The `let mut` cases are the
    /// ones that matter: embassy and ESP bind every OUTPUT that way.
    #[test]
    fn find_pin_binding_line_covers_every_backend_shape() {
        // (source line inside the block, pin, expected binding)
        let cases: [(&str, &str, &str); 4] = [
            // STM32 blocking
            (
                "    let pc13_out_led = gpioc.pc13.into_push_pull_output(&mut gpioc.crh); // GPIO Output",
                "PC13",
                "pc13_out_led",
            ),
            // embassy output (`let mut`)
            (
                "    let mut pa5_out = Output::new(p.PA5, Level::Low, Speed::Low); // GPIO Output",
                "PA5",
                "pa5_out",
            ),
            // embassy input
            (
                "    let pb6_in = Input::new(p.PB6, Pull::None); // GPIO Input",
                "PB6",
                "pb6_in",
            ),
            // ESP output (`let mut`, gpioNN naming)
            (
                "    let mut gpio2_out = Output::new(peripherals.GPIO2, Level::High, OutputConfig::default()); // GPIO Output",
                "GPIO2",
                "gpio2_out",
            ),
        ];
        for (line, pin, binding) in cases {
            let src = format!("#![no_std]\nfn x() {{}}\n{GEN_BEGIN}\n{line}\n{GEN_END}\n");
            assert_eq!(
                find_pin_binding_line(&src, pin),
                // 1: #![no_std], 2: fn x, 3: GEN_BEGIN, 4: the binding
                Some((4, binding.to_owned())),
                "{pin} in `{line}`"
            );
        }
    }

    #[test]
    fn find_pin_binding_line_ignores_non_pin_lets() {
        let src = format!(
            "{GEN_BEGIN}\n\
             let peripherals = esp_hal::init(config);\n\
             let mut gpioc = dp.GPIOC.split();\n\
             let p = embassy_stm32::init(config);\n\
             let mut _adc1 = init_adc1(dp.ADC1, clocks);\n\
             let pc14_out = gpioc.pc14.into_push_pull_output(&mut gpioc.crh); // GPIO Output\n\
             {GEN_END}\n"
        );
        assert_eq!(gen_let_bindings(&src).len(), 1);
        assert_eq!(
            find_pin_binding_line(&src, "PC14"),
            Some((6, "pc14_out".to_owned()))
        );
        assert_eq!(find_pin_binding_line(&src, "PC13"), None);
    }

    /// A pin handed straight to its driver has no `let` of its own — the call
    /// that consumes it is the definition site.
    #[test]
    fn find_pin_mention_line_falls_back_to_the_consuming_call() {
        let src = format!(
            "{GEN_BEGIN}\n\
             let mut _uart1 = Uart::new(peripherals.UART1, cfg)\n\
             .with_rx(peripherals.GPIO20)\n\
             .with_tx(peripherals.GPIO21);\n\
             {GEN_END}\n"
        );
        assert_eq!(find_pin_binding_line(&src, "GPIO20"), None);
        assert_eq!(find_pin_mention_line(&src, "GPIO20"), Some(3));
        assert_eq!(find_pin_mention_line(&src, "GPIO21"), Some(4));
        // Whole-token only: GPIO2 must not light up on GPIO20 / GPIO21.
        assert_eq!(find_pin_mention_line(&src, "GPIO2"), None);
    }

    #[test]
    fn pin_lookups_ignore_code_outside_the_gen_block() {
        let src = format!(
            "{GEN_BEGIN}\n{GEN_END}\n\
             let pc13_out = something(); // GPIO Output\n\
             let x = p.PC13;\n"
        );
        assert_eq!(find_pin_binding_line(&src, "PC13"), None);
        assert_eq!(find_pin_mention_line(&src, "PC13"), None);
    }
}

#[cfg(test)]
mod async_tail_tests {
    use super::{ASYNC_USER_TAIL, USER_TAIL, retarget_pristine_tail};

    /// The warning has to say the two things that make it actionable: WHAT to do
    /// (`.await` every iteration) and WHY (a cooperative scheduler starves the
    /// other tasks). A version that only says "must await" sends the reader
    /// looking for a compiler rule that does not exist.
    #[test]
    fn the_async_tail_carries_the_cooperative_warning() {
        assert!(ASYNC_USER_TAIL.contains("!!! IMPORTANT !!!"));
        assert!(ASYNC_USER_TAIL.contains("Every iteration must `.await`"));
        assert!(ASYNC_USER_TAIL.contains("cooperative"));
        assert!(ASYNC_USER_TAIL.contains("blocks all other tasks"));
        // It opens the loop, before the line the user writes on.
        let loop_at = ASYNC_USER_TAIL.find("loop {").expect("a loop");
        let warn_at = ASYNC_USER_TAIL.find("IMPORTANT").expect("the warning");
        let seed_at = ASYNC_USER_TAIL.find("Your main loop").expect("the seed");
        assert!(loop_at < warn_at && warn_at < seed_at, "{ASYNC_USER_TAIL}");
        // And it is a block comment, closed — an unterminated `/*` would eat
        // the rest of main.
        assert_eq!(ASYNC_USER_TAIL.matches("/*").count(), 1);
        assert_eq!(ASYNC_USER_TAIL.matches("*/").count(), 1);
    }

    /// Both tails must still close `fn main` — they are the only thing that does.
    /// The comment warns that a loop with no await starves every other task —
    /// so the loop it opens must not BE that loop. It ends with one.
    #[test]
    fn the_async_tail_actually_awaits() {
        assert!(ASYNC_USER_TAIL.contains(".await;"), "{ASYNC_USER_TAIL}");
        // Fully qualified, so the tail needs no `use` line to compile. An import
        // in the invariant header would be a second edit somewhere else that a
        // regeneration — or deleting this line — would leave dangling.
        assert!(ASYNC_USER_TAIL.contains("embassy_time::Timer::after"));
        assert!(ASYNC_USER_TAIL.contains("embassy_time::Duration::from_millis"));
        // Last statement in the loop, after the line the user writes on.
        let seed = ASYNC_USER_TAIL.find("Your main loop").expect("the seed");
        let wait = ASYNC_USER_TAIL.find(".await;").expect("the await");
        let close = ASYNC_USER_TAIL.rfind("    }").expect("the closing brace");
        assert!(seed < wait && wait < close, "{ASYNC_USER_TAIL}");
    }

    /// The blocking tail has no executor to yield to — an `.await` there would
    /// not even compile.
    #[test]
    fn the_blocking_tail_does_not_await() {
        assert!(!USER_TAIL.contains(".await"), "{USER_TAIL}");
        assert!(!USER_TAIL.contains("embassy_time"), "{USER_TAIL}");
    }

    #[test]
    fn both_tails_close_the_entry_fn() {
        for tail in [USER_TAIL, ASYNC_USER_TAIL] {
            assert!(tail.trim_end().ends_with("}\n}"), "{tail}");
            assert_eq!(tail.matches("loop {").count(), 1, "{tail}");
        }
    }

    #[test]
    fn a_pristine_tail_is_exchanged_both_ways() {
        assert_eq!(retarget_pristine_tail(USER_TAIL, true), ASYNC_USER_TAIL);
        assert_eq!(retarget_pristine_tail(ASYNC_USER_TAIL, false), USER_TAIL);
    }

    #[test]
    fn a_tail_already_right_for_the_runtime_is_left_alone() {
        assert_eq!(retarget_pristine_tail(USER_TAIL, false), USER_TAIL);
        assert_eq!(
            retarget_pristine_tail(ASYNC_USER_TAIL, true),
            ASYNC_USER_TAIL
        );
    }

    /// The whole point of the guard: the moment there is user code in there, the
    /// tail is theirs. Switching runtime must not rewrite it.
    #[test]
    fn a_tail_the_user_touched_is_never_rewritten() {
        let mine = "    loop {\n        led.toggle();\n    }\n}\n";
        assert_eq!(retarget_pristine_tail(mine, true), mine);
        assert_eq!(retarget_pristine_tail(mine, false), mine);
        // Even one extra line beside the seed counts as touched.
        let plus = "    loop {\n        // Your main loop code here.\n        x();\n    }\n}\n";
        assert_eq!(retarget_pristine_tail(plus, true), plus);
    }

    /// The blank line between `GEN_END` and `loop` must not drift on a switch —
    /// the RP splice passes the tail through untrimmed.
    #[test]
    fn leading_blank_lines_survive_the_exchange() {
        let with_lead = format!("\n\n{USER_TAIL}");
        assert_eq!(
            retarget_pristine_tail(&with_lead, true),
            format!("\n\n{ASYNC_USER_TAIL}")
        );
    }
}

#[cfg(test)]
mod blank_separated_tests {
    use super::blank_separated;

    #[test]
    fn one_blank_line_between_and_none_after_the_last() {
        let out = blank_separated(["a\n".to_owned(), "b\n".to_owned()]);
        assert_eq!(out, "a\n\nb\n");
    }

    /// A caller that builds lines without their newline gets the same result —
    /// the three backends do not agree on which shape they hand over.
    #[test]
    fn an_item_without_a_newline_gets_one() {
        assert_eq!(
            blank_separated(["a".to_owned(), "b".to_owned()]),
            "a\n\nb\n"
        );
    }

    /// A multi-line item stays ONE paragraph: an `#[allow(…)]` and the `let` it
    /// guards must not be split by the separator.
    #[test]
    fn a_multi_line_item_is_not_split() {
        let out = blank_separated([
            "#[allow]\nlet a = 1;\n".to_owned(),
            "let b = 2;\n".to_owned(),
        ]);
        assert_eq!(out, "#[allow]\nlet a = 1;\n\nlet b = 2;\n");
    }

    #[test]
    fn nothing_in_nothing_out() {
        assert_eq!(blank_separated(Vec::<String>::new()), "");
        assert_eq!(blank_separated(["only\n".to_owned()]), "only\n");
    }
}

#[cfg(test)]
mod device_comment_tests {
    use super::{GEN_BEGIN, device_comment, with_device_comment};
    use crate::panels::mcu_module::builtins::builtin_definitions;
    use crate::panels::mcu_module::mcu::Mcu;
    use crate::panels::mcu_module::modules::{ModuleKind, ModuleSignal};

    fn pico() -> Mcu {
        builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu()
    }

    /// A sensor: a UART pair and a spare input line, under one name. The whole
    /// point is that the three read together, so the test asserts they are on
    /// ONE line.
    fn radar() -> (Mcu, usize, usize, usize) {
        let mut mcu = pico();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let tx = mcu.modules[0].pin_for(ModuleSignal::Tx).expect("a TX pad");
        let rx = mcu.modules[0].pin_for(ModuleSignal::Rx).expect("an RX pad");
        let spare = mcu
            .iter_all_pins()
            .find(|p| {
                !p.reserved
                    && p.number != tx
                    && p.number != rx
                    && p.available_functions
                        .contains(&crate::panels::mcu_module::pins::PinFunction::GpioInput)
            })
            .map(|p| p.number)
            .expect("a free input pad");
        mcu.apply_pin_function(
            spare,
            crate::panels::mcu_module::pins::PinFunction::GpioInput,
        );
        let m = mcu.modules[0].clone();
        mcu.join_group_module(&m, "mw radar");
        mcu.join_group(spare, "mw radar");
        (mcu, tx, rx, spare)
    }

    /// A bus whose pads are in "sensors", with two devices: `oled` put in
    /// "display" by hand, `imu` left with its bus.
    fn two_i2c_devices(mcu: &mut Mcu) -> u8 {
        use crate::panels::mcu_module::modules::{I2cDeviceEdit as E, I2cDeviceKey as K};
        assert!(mcu.add_module(ModuleKind::GenericInterfaceI2c));
        let bus = mcu
            .modules
            .iter()
            .find(|m| m.kind == ModuleKind::GenericInterfaceI2c)
            .cloned()
            .expect("the bus");
        let inst = bus.instance();
        mcu.join_group_module(&bus, "sensors");
        assert!(mcu.edit_i2c_device(inst, E::Add));
        assert!(mcu.edit_i2c_device(inst, E::Add));
        let keys: Vec<K> = match &mcu.modules.iter().find(|m| m.id == bus.id).unwrap().config {
            crate::panels::mcu_module::modules::ModuleConfig::I2c(c) => {
                c.rows().iter().map(|r| r.key).collect()
            }
            _ => unreachable!(),
        };
        mcu.edit_i2c_device(inst, E::Name(keys[0], "oled".into()));
        mcu.edit_i2c_device(inst, E::Address(keys[0], 0x3C));
        mcu.edit_i2c_device(inst, E::Name(keys[1], "imu".into()));
        assert!(mcu.join_group_i2c(inst, keys[0], "display"));
        inst
    }

    /// Only a device put in a Device BY HAND is listed: one that is there
    /// because its bus is was never stored, and listing it would rewrite the
    /// file of every project that grouped a bus the moment it was reopened.
    #[test]
    fn only_an_i2c_device_grouped_by_hand_is_listed() {
        let mut mcu = pico();
        let inst = two_i2c_devices(&mut mcu);
        let text = device_comment(&mcu);
        let display = text
            .lines()
            .find(|l| l.starts_with("// display:"))
            .unwrap_or_else(|| panic!("a Device of I2C devices only still has a line: {text}"));
        assert!(
            display.contains(&format!("I2C{inst} oled @ 0x3C")),
            "{display}"
        );
        assert!(
            !text.contains("imu"),
            "a device with its bus is not listed: {text}"
        );
    }

    /// A Device whose only member is a device that was removed since stays
    /// live (an undo brings it back) but has nothing to list: no empty header.
    #[test]
    fn a_device_with_nothing_left_to_list_writes_nothing() {
        let mut mcu = pico();
        let mut g = crate::panels::mcu_module::mcu_config::PinGroup {
            name: "display".into(),
            ..Default::default()
        };
        g.i2c = [(0, 42)].into();
        mcu.groups = vec![g];
        assert!(mcu.groups[0].is_live());
        assert_eq!(device_comment(&mcu), "");
    }

    /// The bus is named the way the family names it - an nRF's is a TWIM.
    #[test]
    fn an_nrf_bus_is_a_twim_in_the_comment() {
        let mut mcu = builtin_definitions()
            .into_iter()
            .find(|d| d.id == "nrf52833_microbit_v2")
            .expect("built-in micro:bit")
            .build_mcu();
        let inst = two_i2c_devices(&mut mcu);
        let text = device_comment(&mcu);
        assert!(text.contains(&format!("TWIM{inst} oled @ 0x3C")), "{text}");
        assert!(!text.contains(&format!("I2C{inst} oled")), "{text}");
    }

    /// Nothing grouped, nothing written. Every existing project is in this case,
    /// and none of them may gain a line.
    #[test]
    fn a_board_with_no_devices_says_nothing() {
        let mcu = pico();
        assert_eq!(device_comment(&mcu), "");
        let code = format!("{GEN_BEGIN}\nuse embassy_rp as _;\n");
        assert_eq!(with_device_comment(code.clone(), &mcu), code);
    }

    /// The three pads of one sensor on one line, each named with what it
    /// carries - the reason the comment exists.
    #[test]
    fn one_device_gathers_its_pads_onto_one_line() {
        let (mcu, tx, rx, spare) = radar();
        let text = device_comment(&mcu);
        let line = text
            .lines()
            .find(|l| l.contains("mw radar"))
            .expect("the device is named");
        for pin in [tx, rx, spare] {
            let name = &mcu.find_pin(pin).expect("the pad").name;
            assert!(line.contains(name.as_str()), "{name} missing from {line:?}");
        }
        assert!(
            line.contains("(IN)"),
            "the spare line says what it is: {line:?}"
        );
        assert!(
            text.lines()
                .all(|l| l.trim().is_empty() || l.starts_with("//")),
            "every line is a comment: {text:?}"
        );
    }

    /// It goes INSIDE the block, on its own line, after the marker.
    ///
    /// Inside, because only that text is rewritten - a comment outside would go
    /// stale the moment a device was renamed. On its own line, because the
    /// marker line is matched exactly by `update_main_rs`.
    #[test]
    fn the_comment_lands_just_inside_the_markers() {
        let (mcu, ..) = radar();
        let code = with_device_comment(
            format!("#![no_std]\n{GEN_BEGIN}\nuse embassy_rp as _;\n"),
            &mcu,
        );
        let lines: Vec<&str> = code.lines().collect();
        // EQUALS, not starts_with: the marker has to keep its own line. Inserted
        // one byte earlier the block would land on the end of the marker line,
        // and `update_main_rs` matches that line to find the block.
        let at = lines
            .iter()
            .position(|l| l.trim() == GEN_BEGIN)
            .expect("the marker kept its own line");
        assert!(lines[at + 1].starts_with("//"), "{:?}", lines[at + 1]);
        assert!(
            lines[at + 1..].iter().any(|l| l.contains("mw radar")),
            "the device is named after the marker, not before"
        );
    }

    /// A file with no block is left alone: a family with no backend generates
    /// nothing, and there is no place to put a comment in a file we did not
    /// write.
    #[test]
    fn a_file_with_no_block_is_untouched() {
        let (mcu, ..) = radar();
        let foreign = "fn main() {}\n".to_owned();
        assert_eq!(with_device_comment(foreign.clone(), &mcu), foreign);
    }

    /// The app rebuilds the block on every save, so inserting has to be
    /// idempotent through the real path - two saves must not stack two
    /// comments.
    #[test]
    fn saving_twice_does_not_stack_two_comments() {
        let (mcu, ..) = radar();
        let once = mcu.fresh_main_rs();
        let twice = mcu.update_main_rs(&once);
        let count = |s: &str| s.matches("Devices on this board").count();
        assert_eq!(count(&once), 1, "the fresh file has it once");
        assert_eq!(count(&twice), 1, "and so does the re-spliced one");
    }

    /// The device name reaches the comment and NOTHING else. A name spliced into
    /// a binding would be re-parsed as part of the pin's label on reopen and
    /// double, and any generated name that moved with the grouping would break
    /// the user's own code below the markers.
    #[test]
    fn the_name_never_reaches_an_identifier() {
        let (mut mcu, ..) = radar();
        let plain = {
            let mut m = mcu.clone();
            m.groups.clear();
            m.fresh_main_rs()
        };
        mcu.rename_group(0, "wildly distinctive name");
        let grouped = mcu.fresh_main_rs();
        let strip = |s: &str| {
            s.lines()
                .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(
            strip(&plain),
            strip(&grouped),
            "grouping changed a line of CODE"
        );
        assert!(grouped.contains("wildly distinctive name"));
    }
}

#[cfg(test)]
mod edge_hook_tests {
    use super::{
        EdgeHook, GEN_BEGIN, GEN_END, USER_TAIL, edge_hook_name, ensure_edge_hooks, hook_seed,
    };

    fn hook(binding: &str) -> EdgeHook {
        EdgeHook {
            name: edge_hook_name(binding),
            what: format!("{binding} pad"),
            caller: format!("{binding}_irq"),
            is_async: true,
        }
    }

    /// A file in the shape every backend leaves: the block, then the tail.
    fn file() -> String {
        format!("{GEN_BEGIN}\nfn main() {{\n    on_p0_14_in_edge(true);\n{GEN_END}\n{USER_TAIL}")
    }

    /// The seed goes in once, below the tail, and a second pass changes
    /// nothing: the hook is the user's from the first write.
    #[test]
    fn seeds_once_below_the_tail_and_is_idempotent() {
        let once = ensure_edge_hooks(file(), &[hook("p0_14_in")]);
        assert_eq!(once.matches("fn on_p0_14_in_edge(").count(), 1, "{once}");
        assert!(
            once.find("fn on_p0_14_in_edge(") > once.find(USER_TAIL),
            "{once}"
        );
        assert!(once.contains("#[allow(clippy::unused_async)]"), "{once}");
        assert_eq!(ensure_edge_hooks(once.clone(), &[hook("p0_14_in")]), once);
    }

    /// The call inside the block is not a definition: it must not stop the
    /// seed, or a project with an armed pin would never get its hook.
    #[test]
    fn the_call_in_the_block_does_not_count_as_present() {
        assert!(
            file().contains("on_p0_14_in_edge(true)"),
            "the fixture calls it"
        );
        let out = ensure_edge_hooks(file(), &[hook("p0_14_in")]);
        assert!(
            out.contains("async fn on_p0_14_in_edge(_high: bool)"),
            "{out}"
        );
    }

    #[test]
    fn nothing_armed_seeds_nothing() {
        assert_eq!(ensure_edge_hooks(file(), &[]), file());
    }

    /// The body the user typed is theirs: a regeneration leaves it byte for
    /// byte, and a second hook lands beside it without touching it.
    #[test]
    fn an_edited_body_is_left_alone_and_a_second_hook_lands_beside_it() {
        let seeded = ensure_edge_hooks(file(), &[hook("p0_14_in")]);
        let edited = seeded.replace(
            "    // Your code here.\n",
            "    FLAG.store(true, Relaxed);\n",
        );
        assert_ne!(edited, seeded, "the edit took");

        let again = ensure_edge_hooks(edited.clone(), &[hook("p0_14_in"), hook("p0_23_in")]);
        assert!(
            again.starts_with(&edited),
            "the file up to the new seed is untouched:\n{again}"
        );
        assert!(again.contains("FLAG.store(true, Relaxed);"), "{again}");
        assert_eq!(again.matches("fn on_p0_23_in_edge(").count(), 1, "{again}");
        assert!(again.find("fn on_p0_23_in_edge(") > again.find("fn on_p0_14_in_edge("));
    }

    /// A hook moved out of main.rs and brought back with a `use` in the tail
    /// is present: seeding another would collide with the `use`. A different
    /// identifier that merely contains the name is not present.
    #[test]
    fn a_use_in_the_tail_counts_and_a_longer_name_does_not() {
        let with_use = format!("{}use handlers::on_p0_14_in_edge;\n", file());
        assert_eq!(
            ensure_edge_hooks(with_use.clone(), &[hook("p0_14_in")]),
            with_use
        );

        let longer = format!("{}fn on_p0_14_in_edge_old(_high: bool) {{}}\n", file());
        let out = ensure_edge_hooks(longer, &[hook("p0_14_in")]);
        assert!(
            out.contains("async fn on_p0_14_in_edge(_high: bool)"),
            "{out}"
        );
    }

    /// The seed lands after a module model (`ensure_module_models` runs
    /// first), still outside `main`.
    #[test]
    fn lands_after_a_module_model() {
        let with_model = format!("{}\n// Data model for x\nmod x {{\n}}\n", file());
        let out = ensure_edge_hooks(with_model, &[hook("p0_14_in")]);
        assert!(
            out.find("fn on_p0_14_in_edge(") > out.find("mod x {"),
            "{out}"
        );
    }

    /// The sync flavor, for a backend whose handler is an interrupt: no
    /// `async`, no clippy allow, and the critical-section warning.
    #[test]
    fn the_sync_seed_has_no_async_and_says_where_it_runs() {
        let seed = hook_seed(&EdgeHook {
            is_async: false,
            ..hook("pa0_in")
        });
        assert!(
            seed.contains("\nfn on_pa0_in_edge(_high: bool) {\n"),
            "{seed}"
        );
        assert!(
            !seed.contains("async") && !seed.contains("allow("),
            "{seed}"
        );
        assert!(seed.contains("critical section: keep it short"), "{seed}");
    }
}
