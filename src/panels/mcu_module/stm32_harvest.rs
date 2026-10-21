//! The STM32 tables the IDE compiles in, harvested again from the crates a
//! generated project builds against.
//!
//! [`super::stm32_flash_geometry`], [`super::stm32_memory_map`] and
//! [`super::stm32_embassy_features`] are generated from stm32-metapac and
//! embassy-stm32 as the local cargo registry holds them, so the IDE and the
//! firmware cannot disagree. Each has one `#[ignore]` test here: it harvests
//! the crate again and compares the result with the committed file, byte for
//! byte; with `EIDE_REGEN=1` it writes the file instead. Ignored because they
//! read outside the repo - build any STM32 project once to unpack the crates.
//! The clock mux table has its own, beside the module that reads it
//! (`codegen::rcc_mux_harvest`).
//!
//! Run them by exact name: a module-path filter would run every ignored test
//! under it, generators included.
//!
//! ```text
//! cargo test -- --ignored --exact panels::mcu_module::stm32_harvest::the_memory_map_matches_metapac
//! $env:EIDE_REGEN = "1"   # PowerShell; then the same command writes the file
//! ```
//!
//! Plain std text scanning of metapac's Rust source, as the other ignored
//! tests do: no new crate, test-only ones included (README counts them).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

// ── The registry ─────────────────────────────────────────────────────────────

/// Every crate unpacked in the cargo registry:
/// `<cargo home>/registry/src/<index>/<name>-<version>`.
pub(crate) fn registry_crates() -> Vec<PathBuf> {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(|h| PathBuf::from(h).join(".cargo"))
        });
    home.map(|h| h.join("registry").join("src"))
        .and_then(|src| std::fs::read_dir(src).ok())
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|index| {
            std::fs::read_dir(index.path())
                .into_iter()
                .flatten()
                .flatten()
        })
        .map(|e| e.path())
        .collect()
}

/// The newest release of `name` whose version starts with `major` (`"21"`,
/// `"0.6"`), by number - `0.6.10` after `0.6.9` - with prereleases skipped.
pub(crate) fn newest_release(name: &str, major: &str) -> Option<PathBuf> {
    let prefix = format!("{name}-{major}.");
    registry_crates()
        .into_iter()
        .filter_map(|path| {
            let rest = path.file_name()?.to_str()?.strip_prefix(prefix.as_str())?;
            let version: Option<Vec<u32>> = rest.split('.').map(|n| n.parse().ok()).collect();
            Some((version?, path))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, path)| path)
}

/// stm32-metapac 21, the release embassy-stm32 0.6 builds against.
pub(crate) fn metapac() -> Option<PathBuf> {
    newest_release("stm32-metapac", "21")
}

/// The crate folder's name, `stm32-metapac-21.0.0`: a table's `Source:`.
fn source_name(dir: &Path) -> String {
    dir.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned()
}

/// `(folder, metadata.rs)` of every chip under metapac's `src/chips`, sorted
/// by folder - the embassy-stm32 feature (`stm32h745zi-cm7`).
pub(crate) fn chips(metapac: &Path) -> Vec<(String, String)> {
    let dir = metapac.join("src").join("chips");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|name| {
            let text = std::fs::read_to_string(dir.join(&name).join("metadata.rs")).ok()?;
            Some((name, text))
        })
        .collect()
}

// ── Check or write ───────────────────────────────────────────────────────────

/// `EIDE_REGEN=1`: the harvest tests write their tables instead of checking.
pub(crate) fn regen() -> bool {
    std::env::var_os("EIDE_REGEN").is_some_and(|v| v == "1")
}

/// The committed `rel` (from the crate root) against a fresh harvest, line
/// endings aside (a worktree checks out CRLF); under `EIDE_REGEN=1` the
/// harvest is written over it, with LF.
pub(crate) fn check_or_write(rel: &str, harvested: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    if regen() {
        std::fs::write(&path, harvested).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        println!("wrote {rel}");
        return;
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .replace("\r\n", "\n");
    if committed == harvested {
        println!("{rel} is the harvest, byte for byte");
        return;
    }
    let (old, new): (Vec<&str>, Vec<&str>) =
        (committed.lines().collect(), harvested.lines().collect());
    let at = old
        .iter()
        .zip(&new)
        .position(|(a, b)| a != b)
        .unwrap_or(old.len().min(new.len()));
    panic!(
        concat!(
            "{} is not what the crate says, from line {}:\n  committed: {:?}\n  harvested: {:?}\n",
            "({} lines committed, {} harvested). Set EIDE_REGEN=1 and run this test again to rewrite it."
        ),
        rel,
        at + 1,
        old.get(at).unwrap_or(&"<end>"),
        new.get(at).unwrap_or(&"<end>"),
        old.len(),
        new.len()
    );
}

// ── cargo check ──────────────────────────────────────────────────────────────

/// The `main.rs` of a check crate: `body` after the bare minimum a `no_std`
/// binary needs.
pub(crate) fn check_main(body: &str) -> String {
    format!(
        "#![no_std]\n#![no_main]\n\n#[panic_handler]\nfn panic(_: &core::panic::PanicInfo) -> ! {{\n    loop {{}}\n}}\n\n{body}"
    )
}

/// `cargo check` of one throw-away crate, `<work>\crates\<name>`, for
/// `target`: `dependency` is its one `[dependencies]` line (the HAL), `main` its
/// `main.rs`. Every crate shares `<work>\target`, so 40-odd chips build the
/// common dependencies once; `lock` seeds a Cargo.lock so they also resolve
/// once. `Err` holds the error lines.
///
/// The compiler is the only oracle for the names this IDE writes into a HAL it
/// does not own: a field, an enum, a variant embassy-stm32 really has.
pub(crate) fn cargo_check_crate(
    work: &Path,
    name: &str,
    dependency: &str,
    target: &str,
    main: &str,
    lock: Option<&Path>,
) -> Result<(), Vec<String>> {
    // Absolute: cargo runs in the crate and would read a relative
    // CARGO_TARGET_DIR from there - a target per crate.
    let work = std::path::absolute(work).expect("a folder path");
    let krate = work.join("crates").join(name);
    std::fs::create_dir_all(krate.join("src")).expect("the crate folder");
    let manifest = format!(
        "[package]\nname = \"clockcheck\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\n{dependency}\n\n[workspace]\n"
    );
    std::fs::write(krate.join("Cargo.toml"), manifest).expect("writable");
    if let Some(lock) = lock
        && !krate.join("Cargo.lock").exists()
    {
        std::fs::copy(lock, krate.join("Cargo.lock")).expect("the seed lock");
    }
    std::fs::write(krate.join("src").join("main.rs"), main).expect("writable");
    let mut cargo = std::process::Command::new("cargo");
    cargo
        .args([
            "check",
            "--quiet",
            "--target",
            target,
            "--message-format",
            "short",
        ])
        .current_dir(&krate)
        .env("CARGO_TARGET_DIR", work.join("target"))
        .env("CARGO_HTTP_MULTIPLEXING", "false")
        // What this test run carries that the crate must not inherit: a
        // pinned toolchain, a build target, and the IDE's own features
        // (rtic-macros counts them).
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("CARGO_BUILD_TARGET");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("CARGO_FEATURE_") {
            cargo.env_remove(key);
        }
    }
    let out = cargo.output().expect("cargo runs");
    if out.status.success() {
        return Ok(());
    }
    Err(String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|l| l.contains(": error") || l.starts_with("error"))
        .map(str::to_owned)
        .collect())
}

// ── metapac's metadata.rs ────────────────────────────────────────────────────

/// The chip's own name, the first `name: "…"` of its metadata.rs
/// (`STM32H743ZI`).
pub(crate) fn chip_name(text: &str) -> &str {
    text.match_indices("name:")
        .find_map(|(at, key)| {
            let rest = text[at + key.len()..].trim_start().strip_prefix('"')?;
            rest.find('"').map(|end| &rest[..end])
        })
        .expect("a chip name")
}

/// The top-level `&[ … ]` groups of `memory: &[ … ]`: one per memory
/// configuration (two where single or dual bank is an option-byte choice).
pub(crate) fn configurations(text: &str) -> Vec<&str> {
    let mem = &text
        [text.find("memory:").expect("memory:")..text.find("peripherals:").expect("peripherals:")];
    let bytes = mem.as_bytes();
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    let mut i = mem.find("&[").expect("a memory list") + 2;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"&[") {
            depth += 1;
            if depth == 1 {
                start = i;
            }
            i += 2;
            continue;
        }
        if bytes[i] == b']' {
            if depth == 1 {
                out.push(&mem[start..i]);
            }
            depth -= 1;
            if depth < 0 {
                break;
            }
        }
        i += 1;
    }
    out
}

/// A flash region's `FlashSettings`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FlashSettings {
    pub erase_size: u64,
    pub write_size: u64,
    pub erase_value: u64,
}

/// One `MemoryRegion` of a configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    pub name: String,
    /// `Flash` or `Ram`.
    pub kind: String,
    pub address: u64,
    pub size: u64,
    pub settings: Option<FlashSettings>,
}

/// The value after `key` in one region, up to its `,`, line end or `}`.
fn value<'a>(region: &'a str, key: &str) -> &'a str {
    let at = region
        .find(key)
        .unwrap_or_else(|| panic!("{key} in {region}"))
        + key.len();
    let v = region[at..].trim_start();
    v[..v.find([',', '\n', '}']).unwrap_or(v.len())]
        .trim()
        .trim_matches('"')
}

/// `0x08000000` or `134217728`.
fn number(text: &str) -> u64 {
    match text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => text.parse(),
    }
    .unwrap_or_else(|e| panic!("{text}: {e}"))
}

/// The regions of one configuration, in metapac's order. Its fields come in
/// the order `name`, `kind`, `address`, `size`, `settings`, so `size:` is the
/// region's own before any `erase_size:`.
pub(crate) fn regions(config: &str) -> Vec<Region> {
    config
        .split("MemoryRegion {")
        .skip(1)
        .map(|r| Region {
            name: value(r, "name:").to_owned(),
            kind: value(r, "kind:")
                .trim_start_matches("MemoryRegionKind::")
                .to_owned(),
            address: number(value(r, "address:")),
            size: number(value(r, "size:")),
            settings: r.contains("FlashSettings {").then(|| FlashSettings {
                erase_size: number(value(r, "erase_size:")),
                write_size: number(value(r, "write_size:")),
                erase_value: number(value(r, "erase_value:")),
            }),
        })
        .collect()
}

// ── Flash geometry ───────────────────────────────────────────────────────────
//
// The Configuration tab's flash store keeps settings in the chip's own flash
// and keeps them out of memory.x's FLASH region. For that the IDE needs, per
// part, facts its chip definitions do not carry: the flash size, the erase
// unit at the END of flash and at its START, and whether the flash is a plain
// row of equal pages that read 0xFF when erased. A rule per family is not
// enough - measured: F0 has 1 KiB and 2 KiB parts at the same size (F071x8),
// the F1 page follows the density, G4 has 2 KiB and 4 KiB lines, H7 128 KiB
// and 8 KiB, and L0/L1 erase to 0x00.
//
// Read from the same metadata embassy-stm32's build script derives its
// `FLASH_SIZE`, `MAX_ERASE_SIZE`, `WRITE_SIZE` and `BANK1_REGION1` from - the
// FIRST memory configuration, its `BANK_*` flash regions (OTP excluded) - and
// the generated code checks it again at compile time.

const UNIFORM: u8 = 1;
const CONTIGUOUS: u8 = 2;
const ERASED_FF: u8 = 4;
const CONFIGURABLE: u8 = 8;

/// `(page, write, flags, head_page, head_size)`: one `Shape` of the table.
type Shape = (u64, u64, u8, u64, u64);

/// `stm32_flash_geometry.rs` as the crate says it, keyed by the chip's name
/// (both cores of a dual-core part are one name, and must agree).
pub(crate) fn flash_geometry(metapac: &Path) -> String {
    let mut parts: BTreeMap<String, (u64, Shape)> = BTreeMap::new();
    for (_, text) in chips(metapac) {
        let name = chip_name(&text).to_ascii_lowercase();
        let configs = configurations(&text);
        let mut banks: Vec<(u64, u64, u64, u64, u64)> = regions(configs[0])
            .into_iter()
            .filter(|r| r.kind == "Flash" && r.name.starts_with("BANK"))
            .filter_map(|r| {
                let s = r.settings?;
                Some((r.address, r.size, s.erase_size, s.write_size, s.erase_value))
            })
            .collect();
        if banks.is_empty() {
            continue; // N6: no internal flash
        }
        banks.sort();
        let mut flags = 0;
        if banks.iter().all(|b| b.2 == banks[0].2) {
            flags |= UNIFORM;
        }
        if banks.windows(2).all(|w| w[0].0 + w[0].1 == w[1].0) {
            flags |= CONTIGUOUS;
        }
        if banks.iter().all(|b| b.4 == 255) {
            flags |= ERASED_FF;
        }
        if configs.len() > 1 {
            flags |= CONFIGURABLE;
        }
        let size = banks.iter().map(|b| b.1).sum();
        let last = banks[banks.len() - 1];
        let write = banks.iter().map(|b| b.3).max().unwrap_or(0);
        let geometry = (size, (last.2, write, flags, banks[0].2, banks[0].1));
        if let Some(other) = parts.get(&name) {
            assert_eq!(*other, geometry, "{name}: two cores disagree on the flash");
        }
        parts.insert(name, geometry);
    }

    let shapes: Vec<Shape> = parts
        .values()
        .map(|g| g.1)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut o = String::new();
    for line in [
        "//! Flash geometry of every STM32 part, harvested from stm32-metapac.",
        "//!",
        "//! GENERATED by `cargo test` (`stm32_harvest`, `EIDE_REGEN=1`) - do not",
        "//! edit by hand.",
    ] {
        o.push_str(line);
        o.push('\n');
    }
    let _ = writeln!(
        o,
        "//! Source: {} — the metadata embassy-stm32 0.6 derives its",
        source_name(metapac)
    );
    o.push_str(concat!(
        "//! `FLASH_SIZE`, `MAX_ERASE_SIZE`, `WRITE_SIZE` and `BANK1_REGION1` from.\n",
        "//!\n",
        "//! `size` is the total of the `BANK_*` regions (OTP excluded), `page` the\n",
        "//! erase size of the region at the END of flash, `write` the write size,\n",
        "//! `head_page` / `head_size` the first region's erase size and size.\n",
        "//! Read through [`super::flash_store`]; `stm32_harvest` says why a rule per\n",
        "//! family would not do.\n",
        "\n",
        "/// One flash layout, shared by every part that has it (sizes aside).\n",
        "#[derive(Clone, Copy, Debug, PartialEq, Eq)]\n",
        "pub struct Shape {\n",
        "    pub page: u32,\n",
        "    pub write: u8,\n",
        "    pub flags: u8,\n",
        "    pub head_page: u32,\n",
        "    pub head_size: u32,\n",
        "}\n",
        "\n",
        "/// Every erase unit is the same size.\n",
    ));
    let _ = writeln!(o, "pub const UNIFORM: u8 = {UNIFORM};");
    o.push_str("/// The banks follow each other with no gap in between.\n");
    let _ = writeln!(o, "pub const CONTIGUOUS: u8 = {CONTIGUOUS};");
    o.push_str("/// Erased bytes read 0xFF (L0/L1 read 0x00).\n");
    let _ = writeln!(o, "pub const ERASED_FF: u8 = {ERASED_FF};");
    o.push_str(concat!(
        "/// Single or dual bank is chosen in the option bytes (two memory\n",
        "/// configurations in metapac); the page size follows that choice.\n",
    ));
    let _ = writeln!(o, "pub const CONFIGURABLE: u8 = {CONFIGURABLE};");
    o.push('\n');
    // rustfmt's own layout, so `cargo fmt` leaves a fresh table alone.
    let _ = writeln!(o, "pub static SHAPES: [Shape; {}] = [", shapes.len());
    for (page, write, flags, head_page, head_size) in &shapes {
        let _ = write!(
            o,
            "    Shape {{\n        page: {page},\n        write: {write},\n        flags: {flags},\n        head_page: {head_page},\n        head_size: {head_size},\n    }},\n"
        );
    }
    o.push_str(concat!(
        "];\n",
        "\n",
        "/// (part, flash size in KiB, index into [`SHAPES`]), sorted by part - the\n",
        "/// part as embassy-stm32 names its feature (`stm32g431cb`).\n",
    ));
    let _ = writeln!(
        o,
        "pub static PARTS: [(&str, u16, u8); {}] = [",
        parts.len()
    );
    for (name, (size, shape)) in &parts {
        assert_eq!(size % 1024, 0, "{name}: flash not whole KiB");
        let index = shapes.binary_search(shape).expect("a listed shape");
        let _ = writeln!(o, "    (\"{name}\", {}, {index}),", size / 1024);
    }
    o.push_str("];\n");
    o
}

// ── Memory map ───────────────────────────────────────────────────────────────
//
// The FLASH and RAM regions memory.x should describe, per part. The importer
// used to write the vendor database's TOTALS at fixed origins - FLASH
// 0x08000000, RAM 0x20000000 - which is wrong on about five hundred parts: an
// STM32H743's 864 KB of RAM is five separate blocks, the one at 0x20000000 is
// the 128 KB DTCM the DMA controllers cannot reach, and a stack the linker put
// at 0x20000000 + 864K sat in unmapped memory (a HardFault before main). A
// U575's 786 KB counts a backup SRAM that is not next to the rest.
//
// So the regions embassy-stm32's own build script reads, picked the way its
// `memory-x` feature picks them (`get_memory_range` + `mem_filter`): the
// LARGEST CONTIGUOUS run of each kind, the WB's radio-core SRAM2 and the
// external memories left out. Two departures: a part with two memory
// configurations gets the single-bank one (the `single-bank` feature the IDE
// writes for exactly those parts), and a dual-core STM32H7, built for its
// Cortex-M7, gets FLASH bank 1 only - its Cortex-M4 boots from bank 2.

/// embassy-stm32 build.rs `mem_filter`, verbatim.
fn mem_filter(chip: &str, region: &str) -> bool {
    let radio_sram = chip.starts_with("STM32WB")
        && !chip.starts_with("STM32WBA")
        && !chip.starts_with("STM32WB0")
        && region.starts_with("SRAM2");
    !(radio_sram
        || region.starts_with("SDRAM_")
        || region.starts_with("FMC_")
        || region.starts_with("OCTOSPI_"))
}

/// `region` with `kind`, sized, by address (stable, as metapac lists them).
fn of_kind<'a>(regions: &'a [Region], kind: &str) -> Vec<&'a Region> {
    let mut out: Vec<&Region> = regions
        .iter()
        .filter(|r| r.kind == kind && r.size != 0)
        .collect();
    out.sort_by_key(|r| r.address);
    out
}

/// embassy-stm32 build.rs `get_memory_range`: the largest contiguous run of
/// `kind`, as (start, length, its regions joined by ` + `).
fn memory_range(chip: &str, regions: &[Region], kind: &str) -> Option<(u64, u64, String)> {
    let (mut start, mut end) = (0u64, None::<u64>);
    let mut names: Vec<&str> = Vec::new();
    let mut best: Option<(u64, u64, String)> = None;
    for r in of_kind(regions, kind) {
        if !mem_filter(chip, &r.name) {
            continue;
        }
        if end != Some(r.address) {
            names.clear();
            start = r.address;
            end = Some(r.address);
        }
        let stop = end.unwrap_or(r.address) + r.size;
        end = Some(stop);
        names.push(&r.name);
        if best.as_ref().is_none_or(|b| stop - start > b.1) {
            best = Some((start, stop - start, names.join(" + ")));
        }
    }
    best
}

/// The filtered contiguous run of `kind` that starts exactly at `base`.
fn run_at(chip: &str, regions: &[Region], kind: &str, base: u64) -> u64 {
    let mut end = base;
    for r in of_kind(regions, kind) {
        if mem_filter(chip, &r.name) && r.address == end {
            end += r.size;
        }
    }
    end - base
}

/// A flash bank that starts INSIDE the one before it is moved to its end.
///
/// stm32-metapac 21 puts the STM32U375/U385's BANK_2 at 0x08010000, inside
/// BANK_1 - CubeMX (db/mcu/memory/STM32_DIE454_*.xml) has one contiguous
/// block with bank 2 at the half. Taken as written the run would stop at bank
/// 1, half the part's flash. Printed, so the correction stays visible.
fn follow_overlaps(feature: &str, regions: Vec<Region>) -> Vec<Region> {
    let mut sorted = regions;
    sorted.sort_by_key(|r| r.address);
    let mut out: Vec<Region> = Vec::new();
    for mut r in sorted {
        if let Some(prev) = out.last()
            && r.kind == prev.kind
            && r.address < prev.address + prev.size
        {
            let moved = prev.address + prev.size;
            println!(
                "  {feature}: {} at 0x{:08X} overlaps {}; taken to follow it at 0x{moved:08X}",
                r.name, r.address, prev.name
            );
            r.address = moved;
        }
        out.push(r);
    }
    out
}

/// `(flash origin, flash KiB, flash regions, flash total KiB, ram origin, ram
/// KiB, ram regions, ram KiB at 0x20000000)`: one `Map` of the table.
type Layout = (u64, u64, String, u64, u64, u64, String, u64);

/// One part's layout, or `None` without internal flash (the N6).
fn layout(feature: &str, text: &str) -> Option<Layout> {
    let chip = chip_name(text);
    let configs = configurations(text);
    let pick = match configs.as_slice() {
        [one] => *one,
        many => {
            let single: Vec<&&str> = many
                .iter()
                .filter(|c| c.contains("BANK_1") && !c.contains("BANK_2"))
                .collect();
            assert_eq!(
                single.len(),
                1,
                "{feature}: {} memory configurations and no single-bank one",
                many.len()
            );
            *single[0]
        }
    };
    let regions = regions(pick);
    let dual_h7 =
        feature.starts_with("stm32h7") && (feature.ends_with("-cm4") || feature.ends_with("-cm7"));
    let flash_regions = follow_overlaps(
        feature,
        regions
            .iter()
            .filter(|r| r.kind == "Flash" && !(dual_h7 && r.name.starts_with("BANK_2")))
            .cloned()
            .collect(),
    );
    let flash = memory_range(chip, &flash_regions, "Flash");
    // Every bank of the configuration, BANK_2 included on a dual-core H7: a
    // saved 2048K there is the vendor total, and is cut to bank 1 - the total
    // says it is the part's flash, the run says how much is linked.
    let flash_total: u64 = regions
        .iter()
        .filter(|r| r.kind == "Flash" && r.name.starts_with("BANK"))
        .map(|r| r.size)
        .sum();
    let ram = memory_range(chip, &regions, "Ram");
    let (flash, ram) = (flash?, ram?);
    // Whole KiB, rounded DOWN as embassy's `size / 1024` does: the WB0's
    // 24552-byte SRAM is the one region that is not a multiple.
    Some((
        flash.0,
        flash.1 / 1024,
        flash.2,
        flash_total / 1024,
        ram.0,
        ram.1 / 1024,
        ram.2,
        run_at(chip, &regions, "Ram", 0x2000_0000) / 1024,
    ))
}

/// `stm32_memory_map.rs` as the crate says it, keyed by chip folder - the
/// embassy-stm32 feature (`stm32h743zi`, `stm32l151c8-a`, `stm32h745zi-cm7`).
pub(crate) fn memory_map(metapac: &Path) -> String {
    let mut parts: BTreeMap<String, Layout> = BTreeMap::new();
    let mut skipped = Vec::new();
    for (feature, text) in chips(metapac) {
        match layout(&feature, &text) {
            Some(l) => {
                parts.insert(feature, l);
            }
            None => skipped.push(feature),
        }
    }
    println!("no flash: {}", skipped.join(", "));
    let maps: Vec<Layout> = parts
        .values()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let mut o = String::from(concat!(
        "//! The FLASH and RAM regions memory.x describes, per STM32 part.\n",
        "//!\n",
        "//! GENERATED by `cargo test` (`stm32_harvest`, `EIDE_REGEN=1`) - do not\n",
        "//! edit by hand.\n",
    ));
    let _ = writeln!(
        o,
        "//! Source: {} - the regions embassy-stm32's build script reads,",
        source_name(metapac)
    );
    o.push_str(concat!(
        "//! picked the way its `memory-x` feature picks them: the largest contiguous\n",
        "//! run of each kind (the WB's radio-core SRAM2 and external memories left\n",
        "//! out). A two-configuration part gets its single-bank one, and a dual-core\n",
        "//! STM32H7 FLASH bank 1 only; `stm32_harvest` says why.\n",
        "\n",
        "/// One memory layout, shared by every part that has it.\n",
        "#[derive(Clone, Copy, Debug, PartialEq, Eq)]\n",
        "pub struct Map {\n",
        "    pub flash_origin: u32,\n",
        "    pub flash_kib: u32,\n",
        "    /// The metapac regions the FLASH run is made of (`BANK_1 + BANK_2`).\n",
        "    pub flash_regions: &'static str,\n",
        "    /// Every flash bank of the part, run or not: a saved FLASH size no\n",
        "    /// larger than this is one the chip has, and stays.\n",
        "    pub flash_total_kib: u32,\n",
        "    pub ram_origin: u32,\n",
        "    pub ram_kib: u32,\n",
        "    pub ram_regions: &'static str,\n",
        "    /// The RAM that runs on from 0x20000000 - what the old importer's\n",
        "    /// `0x20000000 + total` assumed was there, so what a saved definition\n",
        "    /// is checked against.\n",
        "    pub ram_kib_at_base: u32,\n",
        "}\n",
        "\n",
    ));
    let _ = writeln!(o, "pub static MAPS: [Map; {}] = [", maps.len());
    for (fo, fk, fr, ft, ro, rk, rr, rb) in &maps {
        let _ = write!(
            o,
            "    Map {{\n        flash_origin: 0x{fo:08X},\n        flash_kib: {fk},\n        flash_regions: \"{fr}\",\n        flash_total_kib: {ft},\n        ram_origin: 0x{ro:08X},\n        ram_kib: {rk},\n        ram_regions: \"{rr}\",\n        ram_kib_at_base: {rb},\n    }},\n"
        );
    }
    o.push_str(concat!(
        "];\n",
        "\n",
        "/// (embassy-stm32 chip feature, index into [`MAPS`]), sorted by feature.\n",
    ));
    let _ = writeln!(o, "pub static PARTS: [(&str, u16); {}] = [", parts.len());
    for (feature, layout) in &parts {
        let index = maps.binary_search(layout).expect("a listed map");
        let _ = writeln!(o, "    (\"{feature}\", {index}),");
    }
    o.push_str("];\n");
    o
}

// ── embassy-stm32's chip features ────────────────────────────────────────────
//
// A generated STM32 project names its chip as an embassy-stm32 Cargo feature,
// and a feature the crate does not publish makes the whole manifest
// unresolvable: cargo refuses it, and rust-analyzer dies with it. The IDE
// derives the feature from the part number, so it needs the published list to
// check that derivation offline - at import, in the chip search and in the
// tests - instead of asking the crates.io index with a 4 s budget. Re-run
// when `EMBASSY_VERSION` moves.

/// A chip key at column 0 of `[features]`: `stm32`, lower-case letters, a
/// digit, then `[a-z0-9-]*`, then `=` (`stm32h745zi-cm7 = [`; not
/// `stm32-hrtim`).
fn chip_feature_key(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("stm32")?;
    let letters = rest.bytes().take_while(u8::is_ascii_lowercase).count();
    if letters == 0 || !rest[letters..].starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let tail = rest[letters..]
        .bytes()
        .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        .count();
    let key = &line[..5 + letters + tail];
    line[key.len()..]
        .trim_start()
        .starts_with('=')
        .then_some(key)
}

/// `stm32_embassy_features.rs` from the `[features]` table of `crate_dir`'s
/// Cargo.toml.
pub(crate) fn embassy_features(crate_dir: &Path) -> String {
    let manifest = std::fs::read_to_string(crate_dir.join("Cargo.toml"))
        .unwrap_or_else(|e| panic!("{}: {e}", crate_dir.display()));
    let mut features = BTreeSet::new();
    let mut inside = false;
    for line in manifest.lines() {
        if line.starts_with('[') {
            inside = line.trim() == "[features]";
            continue;
        }
        if inside && let Some(key) = chip_feature_key(line) {
            features.insert(key);
        }
    }
    assert!(
        !features.is_empty(),
        "no chip features in {}",
        crate_dir.display()
    );
    let mut o = String::from(concat!(
        "//! Every chip feature embassy-stm32 publishes, harvested from its Cargo.toml.\n",
        "//!\n",
        "//! GENERATED by `cargo test` (`stm32_harvest`, `EIDE_REGEN=1`) - do not\n",
        "//! edit by hand.\n",
    ));
    let _ = writeln!(
        o,
        "//! Source: {} - the release `EMBASSY_VERSION` resolves to here.",
        source_name(crate_dir)
    );
    o.push_str(concat!(
        "//!\n",
        "//! Sorted, for the binary search in\n",
        "//! [`super::stm32_pin_data::embassy_publishes`]. Per-core parts appear only\n",
        "//! with their core (`stm32h745zi-cm7`), and the STM32L1 die variants with\n",
        "//! theirs (`stm32l151c6-a`, `stm32l151vd-x`) next to the bare die.\n",
        "\n",
    ));
    let _ = writeln!(o, "pub static FEATURES: [&str; {}] = [", features.len());
    for f in &features {
        let _ = writeln!(o, "    \"{f}\",");
    }
    o.push_str("];\n");
    o
}

// ── The tests ────────────────────────────────────────────────────────────────

/// stm32-metapac 21, or the test fails: a harvest that skipped would read as a
/// table checked - or, under `EIDE_REGEN=1`, written - when nothing was.
pub(crate) fn metapac_or_fail() -> PathBuf {
    metapac().expect("stm32-metapac 21 in the cargo registry: build any STM32 project once")
}

/// `stm32_flash_geometry.rs` against stm32-metapac 21.
#[test]
#[ignore]
fn the_flash_geometry_matches_metapac() {
    let metapac = metapac_or_fail();
    check_or_write(
        "src/panels/mcu_module/stm32_flash_geometry.rs",
        &flash_geometry(&metapac),
    );
}

/// `stm32_memory_map.rs` against stm32-metapac 21.
#[test]
#[ignore]
fn the_memory_map_matches_metapac() {
    let metapac = metapac_or_fail();
    check_or_write(
        "src/panels/mcu_module/stm32_memory_map.rs",
        &memory_map(&metapac),
    );
}

/// `stm32_embassy_features.rs` against the embassy-stm32 release
/// `EMBASSY_VERSION` resolves to in the cargo registry.
#[test]
#[ignore]
fn the_embassy_feature_list_matches_the_registry() {
    use super::stm32_pin_data::{EMBASSY_CRATE, EMBASSY_VERSION};
    let dir = newest_release(EMBASSY_CRATE, EMBASSY_VERSION).unwrap_or_else(|| {
        panic!("{EMBASSY_CRATE} {EMBASSY_VERSION}.x is not in the cargo registry: build any STM32 project once")
    });
    check_or_write(
        "src/panels/mcu_module/stm32_embassy_features.rs",
        &embassy_features(&dir),
    );
}

/// The pieces the harvests stand on, on text shaped like metapac's.
#[test]
fn the_metadata_reader_reads_metapacs_shape() {
    let text = r#"pub(crate) static METADATA: Metadata = Metadata {
    name: "STM32G474RE",
    family: "STM32G4",
    memory: &[
        &[
            MemoryRegion {
                name: "BANK_1",
                kind: MemoryRegionKind::Flash,
                address: 134217728,
                size: 524288,
                settings: Some(FlashSettings {
                    erase_size: 4096,
                    write_size: 8,
                    erase_value: 255,
                }),
            },
            MemoryRegion {
                name: "SRAM",
                kind: MemoryRegionKind::Ram,
                address: 0x20000000,
                size: 98304,
                settings: None,
            },
        ],
        &[
            MemoryRegion {
                name: "BANK_1",
                kind: MemoryRegionKind::Flash,
                address: 134217728,
                size: 262144,
                settings: Some(FlashSettings {
                    erase_size: 2048,
                    write_size: 8,
                    erase_value: 255,
                }),
            },
            MemoryRegion {
                name: "BANK_2",
                kind: MemoryRegionKind::Flash,
                address: 134479872,
                size: 262144,
                settings: Some(FlashSettings {
                    erase_size: 2048,
                    write_size: 8,
                    erase_value: 255,
                }),
            },
        ],
    ],
    peripherals: &[],
};"#;
    assert_eq!(chip_name(text), "STM32G474RE");
    let configs = configurations(text);
    assert_eq!(configs.len(), 2);
    let first = regions(configs[0]);
    assert_eq!(first.len(), 2);
    assert_eq!(
        first[0],
        Region {
            name: "BANK_1".into(),
            kind: "Flash".into(),
            address: 0x0800_0000,
            size: 512 * 1024,
            settings: Some(FlashSettings {
                erase_size: 4096,
                write_size: 8,
                erase_value: 255
            }),
        }
    );
    assert_eq!(
        (first[1].kind.as_str(), first[1].address, first[1].settings),
        ("Ram", 0x2000_0000, None)
    );
    // The single-bank configuration is the one with BANK_1 and no BANK_2.
    let l = layout("stm32g474re", text).expect("flash and RAM");
    assert_eq!(
        (l.0, l.1, l.2.as_str(), l.3),
        (0x0800_0000, 512, "BANK_1", 512)
    );
    assert_eq!((l.4, l.5, l.6.as_str(), l.7), (0x2000_0000, 96, "SRAM", 96));

    // The largest contiguous run, its regions named; a gap starts a new one,
    // and a filtered region (an FMC bank) is skipped without breaking it.
    let r = |name: &str, address: u64, size: u64| Region {
        name: name.into(),
        kind: "Ram".into(),
        address,
        size,
        settings: None,
    };
    let rams = [
        r("DTCM", 0x2000_0000, 128 * 1024),
        r("AXISRAM", 0x2400_0000, 512 * 1024),
        r("SRAM1", 0x3000_0000, 128 * 1024),
        r("SRAM2", 0x3002_0000, 128 * 1024),
        r("FMC_SDRAM", 0x3004_0000, 1 << 30),
        r("SRAM3", 0x3004_0000, 32 * 1024),
    ];
    assert_eq!(
        memory_range("STM32H743ZI", &rams, "Ram"),
        Some((0x2400_0000, 512 * 1024, "AXISRAM".into()))
    );
    assert_eq!(run_at("STM32H743ZI", &rams, "Ram", 0x2000_0000), 128 * 1024);
    assert_eq!(run_at("STM32H743ZI", &rams, "Ram", 0x3000_0000), 288 * 1024);
    // The WB's radio-core SRAM2 is left out; the WBA's is not.
    assert!(!mem_filter("STM32WB55RG", "SRAM2a"));
    assert!(mem_filter("STM32WBA55CG", "SRAM2"));

    // A bank starting inside the one before it follows it.
    let flash = |name: &str, address: u64, size: u64| Region {
        kind: "Flash".into(),
        ..r(name, address, size)
    };
    let moved = follow_overlaps(
        "stm32u385rg",
        vec![
            flash("BANK_1", 0x0800_0000, 512 * 1024),
            flash("BANK_2", 0x0801_0000, 512 * 1024),
        ],
    );
    assert_eq!(moved[1].address, 0x0808_0000);

    // The chip keys of `[features]`, and nothing else.
    assert_eq!(
        chip_feature_key("stm32h745zi-cm7 = ["),
        Some("stm32h745zi-cm7")
    );
    assert_eq!(
        chip_feature_key("stm32l151c6-a = [\"x\"]"),
        Some("stm32l151c6-a")
    );
    assert_eq!(chip_feature_key("stm32f103c8=[]"), Some("stm32f103c8"));
    for not in [
        "stm32-hrtim = []",
        "stm32f = []",
        " stm32f103c8 = []",
        "stm32f103c8 [",
        "rt = []",
    ] {
        assert_eq!(chip_feature_key(not), None, "{not}");
    }
}
