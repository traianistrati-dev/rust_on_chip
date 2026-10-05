//! Flash/RAM usage measurement — builds `--release` and reads the ELF itself.
//!
//! `start_measure` runs `cargo build --release --message-format=json` in the
//! check workspace (fast when a flash build already ran — cargo caches), takes
//! the executable path from cargo's `compiler-artifact` message, then parses
//! the ELF program + section headers directly — no external `size`/`objdump`
//! tool needed:
//!  - Flash use = Σ file bytes of PT_LOAD segments (what gets programmed:
//!    .vector_table + .text + .rodata + the .data initializers)
//!  - RAM use   = Σ memory bytes of PT_LOAD segments inside the RAM region
//!    (static .data + .bss + .uninit — stack and heap come ON TOP of this)
//!
//! Region limits come from the project's `memory.x`. Without one (ESP32 —
//! esp-hal's build script owns the layout) usage is classified by the
//! segments' write flag and shown without percentages.

use std::{
    io::BufRead,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};

use crate::build::no_window;

// ── Memory regions (memory.x) ─────────────────────────────────────────────────

/// One `MEMORY` region: `ORIGIN = origin, LENGTH = length` (bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemRegion {
    pub origin: u64,
    pub length: u64,
}

impl MemRegion {
    fn contains(&self, addr: u64) -> bool {
        addr >= self.origin && addr < self.origin.saturating_add(self.length)
    }
}

/// The FLASH + RAM regions parsed from `memory.x` (either may be missing —
/// ESP32 projects have no memory.x at all).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemLimits {
    pub flash: Option<MemRegion>,
    pub ram: Option<MemRegion>,
}

/// Parse the `MEMORY { … }` regions out of a `memory.x` linker script.
/// Regions named `FLASH*` fill `flash`, `RAM*`/`SRAM*` fill `ram` (first match
/// wins). Values accept ld syntax: hex (`0x08000000`), decimal, `K`/`M` suffix.
pub fn parse_memory_x(text: &str) -> MemLimits {
    let mut limits = MemLimits::default();
    // Strip /* … */ comments so a commented-out region is not picked up.
    let mut clean = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        clean.push_str(&rest[..start]);
        match rest[start..].find("*/") {
            Some(end) => rest = &rest[start + end + 2..],
            None => {
                rest = "";
                break;
            }
        }
    }
    clean.push_str(rest);

    for line in clean.lines() {
        let Some(colon) = line.find(':') else {
            continue;
        };
        let name = line[..colon]
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .find(|t| !t.is_empty())
            .unwrap_or("")
            .to_ascii_uppercase();
        let spec = &line[colon + 1..];
        let (Some(origin), Some(length)) = (
            field_value(spec, "ORIGIN").and_then(parse_ld_number),
            field_value(spec, "LENGTH").and_then(parse_ld_number),
        ) else {
            continue;
        };
        let region = MemRegion { origin, length };
        if name.starts_with("FLASH") && limits.flash.is_none() {
            limits.flash = Some(region);
        } else if (name.starts_with("RAM") || name.starts_with("SRAM")) && limits.ram.is_none() {
            limits.ram = Some(region);
        }
    }
    limits
}

/// The value token following `<key> =` in an ld region spec — e.g.
/// `field_value("ORIGIN = 0x0800000, LENGTH = 64K", "LENGTH")` → `"64K"`.
fn field_value<'a>(spec: &'a str, key: &str) -> Option<&'a str> {
    let at = spec.find(key)?;
    let after = spec[at + key.len()..].trim_start();
    let after = after.strip_prefix('=')?.trim_start();
    let end = after
        .find(|c: char| c == ',' || c.is_whitespace())
        .unwrap_or(after.len());
    Some(&after[..end])
}

/// An ld number: `0x…` hex, decimal, optional `K`/`M` multiplier suffix.
pub(crate) fn parse_ld_number(tok: &str) -> Option<u64> {
    let tok = tok.trim();
    let (tok, mult) = match tok.chars().last() {
        Some('K') | Some('k') => (&tok[..tok.len() - 1], 1024u64),
        Some('M') | Some('m') => (&tok[..tok.len() - 1], 1024 * 1024),
        _ => (tok, 1),
    };
    let value = if let Some(hex) = tok.strip_prefix("0x").or_else(|| tok.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()?
    } else {
        tok.parse::<u64>().ok()?
    };
    Some(value * mult)
}

// ── ELF parsing ───────────────────────────────────────────────────────────────

/// One ALLOC section for the breakdown tooltip (.text, .rodata, .data, .bss…).
/// `.data` counts in BOTH: its initializers live in flash, its bytes in RAM.
#[derive(Clone, Debug)]
pub struct SectionUse {
    pub name: String,
    pub size: u64,
    pub in_flash: bool,
    pub in_ram: bool,
}

/// The measured usage of one built ELF.
#[derive(Clone, Debug)]
pub struct MemUsage {
    pub flash_used: u64,
    pub ram_used: u64,
    pub limits: MemLimits,
    pub sections: Vec<SectionUse>,
}

const PT_LOAD: u32 = 1;
const PF_W: u32 = 2;
const SHT_NOBITS: u32 = 8;
const SHF_WRITE: u64 = 1;
const SHF_ALLOC: u64 = 2;

/// Little-endian bounds-checked readers (a truncated/corrupt ELF must error,
/// never panic the worker thread).
fn rd16(b: &[u8], at: usize) -> Result<u64, String> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]) as u64)
        .ok_or_else(|| "ELF truncated".into())
}
fn rd32(b: &[u8], at: usize) -> Result<u64, String> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as u64)
        .ok_or_else(|| "ELF truncated".into())
}
fn rd64(b: &[u8], at: usize) -> Result<u64, String> {
    b.get(at..at + 8)
        .map(|s| u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
        .ok_or_else(|| "ELF truncated".into())
}

/// Parse the ELF and classify its load segments / alloc sections against the
/// memory.x `limits`. Handles ELF32 and ELF64, little-endian (Cortex-M and
/// ESP32 RISC-V are both ELF32 LE; 64-bit covered for host-side binaries).
pub fn parse_elf(bytes: &[u8], limits: MemLimits) -> Result<MemUsage, String> {
    if bytes.len() < 52 || bytes[..4] != [0x7f, b'E', b'L', b'F'] {
        return Err("not an ELF file".into());
    }
    let is64 = match bytes[4] {
        1 => false,
        2 => true,
        c => return Err(format!("unknown ELF class {c}")),
    };
    if bytes[5] != 1 {
        return Err("big-endian ELF not supported".into());
    }

    // Header field offsets differ between ELF32 and ELF64.
    let (phoff, phentsize, phnum, shoff, shentsize, shnum, shstrndx) = if is64 {
        (
            rd64(bytes, 32)? as usize,
            rd16(bytes, 54)? as usize,
            rd16(bytes, 56)? as usize,
            rd64(bytes, 40)? as usize,
            rd16(bytes, 58)? as usize,
            rd16(bytes, 60)? as usize,
            rd16(bytes, 62)? as usize,
        )
    } else {
        (
            rd32(bytes, 28)? as usize,
            rd16(bytes, 42)? as usize,
            rd16(bytes, 44)? as usize,
            rd32(bytes, 32)? as usize,
            rd16(bytes, 46)? as usize,
            rd16(bytes, 48)? as usize,
            rd16(bytes, 50)? as usize,
        )
    };

    // ── Program headers (PT_LOAD segments) → the authoritative totals ────────
    let mut flash_used = 0u64;
    let mut ram_used = 0u64;
    for i in 0..phnum {
        let at = phoff + i * phentsize;
        let (p_type, vaddr, filesz, memsz, flags) = if is64 {
            (
                rd32(bytes, at)? as u32,
                rd64(bytes, at + 16)?,
                rd64(bytes, at + 32)?,
                rd64(bytes, at + 40)?,
                rd32(bytes, at + 4)? as u32,
            )
        } else {
            (
                rd32(bytes, at)? as u32,
                rd32(bytes, at + 8)?,
                rd32(bytes, at + 16)?,
                rd32(bytes, at + 20)?,
                rd32(bytes, at + 24)? as u32,
            )
        };
        if p_type != PT_LOAD {
            continue;
        }
        // Every loadable file byte ends up in the programmed image.
        flash_used += filesz;
        // Static RAM: segments living in the RAM region (memory.x known), or —
        // without limits — writable segments (.data + .bss).
        let in_ram = match limits.ram {
            Some(r) => r.contains(vaddr),
            None => flags & PF_W != 0,
        };
        if in_ram {
            ram_used += memsz;
        }
    }

    // ── Section breakdown (best-effort — totals never depend on it) ──────────
    let mut sections = Vec::new();
    // (name, address, size) of every ALLOC section, for the hole below.
    let mut spans: Vec<(String, u64, u64)> = Vec::new();
    let read_section = |at: usize| -> Result<(u64, u32, u64, u64, u64), String> {
        // (name_off, type, flags, addr, size)
        if is64 {
            Ok((
                rd32(bytes, at)?,
                rd32(bytes, at + 4)? as u32,
                rd64(bytes, at + 8)?,
                rd64(bytes, at + 16)?,
                rd64(bytes, at + 32)?,
            ))
        } else {
            Ok((
                rd32(bytes, at)?,
                rd32(bytes, at + 4)? as u32,
                rd32(bytes, at + 8)?,
                rd32(bytes, at + 12)?,
                rd32(bytes, at + 20)?,
            ))
        }
    };
    if shnum > 0 && shstrndx < shnum {
        // The .shstrtab section holds every section's name.
        let strtab_hdr = shoff + shstrndx * shentsize;
        let strtab_off = if is64 {
            rd64(bytes, strtab_hdr + 24)? as usize
        } else {
            rd32(bytes, strtab_hdr + 16)? as usize
        };
        for i in 0..shnum {
            let (name_off, sh_type, sh_flags, addr, size) = read_section(shoff + i * shentsize)?;
            if sh_flags & SHF_ALLOC == 0 || size == 0 {
                continue;
            }
            let name_at = strtab_off + name_off as usize;
            let name: String = bytes
                .get(name_at..)
                .map(|s| {
                    s.iter()
                        .take_while(|&&c| c != 0)
                        .map(|&c| c as char)
                        .collect()
                })
                .unwrap_or_default();
            // .data-style sections (PROGBITS at a RAM address) count in both:
            // initializers in flash, live bytes in RAM. NOBITS (.bss) is RAM only.
            let in_ram = match limits.ram {
                Some(r) => r.contains(addr),
                None => sh_flags & SHF_WRITE != 0 || sh_type == SHT_NOBITS,
            };
            let in_flash = sh_type != SHT_NOBITS;
            spans.push((name.clone(), addr, size));
            sections.push(SectionUse {
                name,
                size,
                in_flash,
                in_ram,
            });
        }
    }

    // The bar's capacity is what the PROGRAM can use: FLASH less the hole a
    // memory.x leaves before `_stext` (the rest of sector 0 and an F2/F4/F7
    // flash store), as an end-of-flash store's shorter LENGTH already is.
    let limits = MemLimits {
        flash: limits.flash.map(|f| MemRegion {
            length: f.length.saturating_sub(hole_after_vectors(f, &spans)),
            ..f
        }),
        ..limits
    };

    Ok(MemUsage {
        flash_used,
        ram_used,
        limits,
        sections,
    })
}

/// The bytes in `flash` between the end of `.vector_table` and the next
/// section: 0 in cortex-m-rt's own layout, where `.text` follows the table;
/// the rest of sector 0 plus the store when memory.x starts the program past
/// a Configuration-tab flash store (`_stext`). Nothing is loaded there, so the
/// used bytes never count it, and lld still charges it to FLASH.
fn hole_after_vectors(flash: MemRegion, spans: &[(String, u64, u64)]) -> u64 {
    let Some(table_end) = spans
        .iter()
        .find(|(name, ..)| name == ".vector_table")
        .map(|(_, addr, size)| addr + size)
        .filter(|&end| end > flash.origin && flash.contains(end - 1))
    else {
        return 0;
    };
    spans
        .iter()
        .map(|&(_, addr, _)| addr)
        .filter(|&addr| addr >= table_end && flash.contains(addr))
        .min()
        .map_or(0, |next| next - table_end)
}

// ── State + runner ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub enum SizeState {
    #[default]
    Idle,
    /// `cargo build --release` is running.
    Building,
    Done(MemUsage),
    Failed(String),
}

impl SizeState {
    pub fn is_busy(&self) -> bool {
        matches!(self, SizeState::Building)
    }
}

/// Build `--release` in `project_dir` and measure the resulting ELF against
/// the `memory_x` limits (empty for ESP32 → sizes without percentages).
/// Runs on a background thread; progress lands in `state`.
pub fn start_measure(
    project_dir: PathBuf,
    target: String,
    memory_x: String,
    state: Arc<Mutex<SizeState>>,
    ctx: eframe::egui::Context,
    activity: Arc<Mutex<crate::activity::ActivityLog>>,
) {
    if state.lock().unwrap().is_busy() {
        return;
    }
    *state.lock().unwrap() = SizeState::Building;
    ctx.request_repaint();

    thread::spawn(move || {
        // Commits the timing breakdown on drop — every early return still logs.
        let mut act = crate::activity::Committing::new("Size (Flash/RAM)", activity);
        let next = run_measure(&project_dir, &target, &memory_x, &mut act);
        *state.lock().unwrap() = next;
        ctx.request_repaint();
    });
}

/// The worker: `cargo build --release --message-format=json`, read the
/// executable path from the artifact messages, parse the ELF.
fn run_measure(
    project_dir: &std::path::Path,
    target: &str,
    memory_x: &str,
    act: &mut crate::activity::Committing,
) -> SizeState {
    let cmd_str = format!("cargo build --release --target {target} --message-format=json");
    let t_build = std::time::Instant::now();
    let mut child = match no_window(&mut Command::new("cargo"))
        .current_dir(project_dir)
        .args([
            "build",
            "--release",
            "--target",
            target,
            "--message-format=json",
            "--color=never",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return SizeState::Failed(format!("Could not launch `cargo`: {e}")),
    };

    let mut executable: Option<PathBuf> = None;
    let mut success = false;
    let mut errors: Vec<String> = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        for line in std::io::BufReader::new(stdout).lines().flatten() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            match v["reason"].as_str() {
                Some("compiler-artifact") => {
                    if let Some(exe) = v["executable"].as_str() {
                        executable = Some(PathBuf::from(exe));
                    }
                }
                Some("compiler-message") => {
                    if v["message"]["level"].as_str() == Some("error") {
                        if let Some(r) = v["message"]["rendered"].as_str() {
                            errors.push(r.to_string());
                        }
                    }
                }
                Some("build-finished") => {
                    success = v["success"].as_bool().unwrap_or(false);
                }
                _ => {}
            }
        }
    }
    let exit = child.wait().ok().and_then(|s| s.code());
    act.rec()
        .cmd_phase("cargo build --release", cmd_str, t_build.elapsed(), exit);

    if !success {
        let detail = errors.first().cloned().unwrap_or_else(|| {
            "cargo build --release failed — run Build for the full diagnostics".to_string()
        });
        return SizeState::Failed(detail);
    }
    let Some(elf_path) = executable else {
        return SizeState::Failed("build produced no executable artifact".into());
    };

    let t_parse = std::time::Instant::now();
    let result = std::fs::read(&elf_path)
        .map_err(|e| format!("cannot read {}: {e}", elf_path.display()))
        .and_then(|bytes| parse_elf(&bytes, parse_memory_x(memory_x)));
    act.rec().cmd_phase(
        "parse ELF",
        format!("read {}", elf_path.display()),
        t_parse.elapsed(),
        Some(if result.is_ok() { 0 } else { 1 }),
    );
    match result {
        Ok(usage) => SizeState::Done(usage),
        Err(e) => SizeState::Failed(e),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_x_parses_the_generated_layout() {
        let text = "/* STM32F103C8 — 64K flash, 20K RAM */\n\
                    MEMORY\n\
                    {\n\
                        FLASH : ORIGIN = 0x08000000, LENGTH = 64K\n\
                        RAM   : ORIGIN = 0x20000000,   LENGTH = 20K\n\
                    }\n";
        let l = parse_memory_x(text);
        assert_eq!(
            l.flash,
            Some(MemRegion {
                origin: 0x0800_0000,
                length: 64 * 1024
            })
        );
        assert_eq!(
            l.ram,
            Some(MemRegion {
                origin: 0x2000_0000,
                length: 20 * 1024
            })
        );
    }

    /// User-edited layouts: attribute lists after the region name, decimal and
    /// `M` sizes, lowercase suffix, extra spaces.
    #[test]
    fn memory_x_parses_edited_variants() {
        let text = "MEMORY\n{\n\
                    FLASH (rx) : ORIGIN = 0x08000000, LENGTH = 1M\n\
                    SRAM (rwx) : ORIGIN=0x20000000,LENGTH=131072\n}\n";
        let l = parse_memory_x(text);
        assert_eq!(l.flash.unwrap().length, 1024 * 1024);
        assert_eq!(l.ram.unwrap().length, 128 * 1024);
        // A commented-out region must not win over the live one.
        let text2 = "MEMORY {\n/* FLASH : ORIGIN = 0x00000000, LENGTH = 1K */\n\
                     FLASH : ORIGIN = 0x08000000, LENGTH = 64k\n}";
        assert_eq!(parse_memory_x(text2).flash.unwrap().origin, 0x0800_0000);
    }

    #[test]
    fn empty_memory_x_yields_no_limits() {
        assert_eq!(parse_memory_x(""), MemLimits::default());
    }

    // ── Synthetic-ELF helpers ─────────────────────────────────────────────────

    fn push32(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn push16(v: &mut Vec<u8>, x: u16) {
        v.extend_from_slice(&x.to_le_bytes());
    }

    /// A minimal ELF32 LE with the classic Cortex-M layout:
    /// .text (flash), .data (VMA in RAM, LMA in flash), .bss (RAM, NOBITS).
    fn cortex_m_style_elf() -> Vec<u8> {
        let mut b = Vec::new();
        // e_ident: magic, class 1 (32-bit), data 1 (LE), version 1
        b.extend_from_slice(&[0x7f, b'E', b'L', b'F', 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        push16(&mut b, 2); // e_type EXEC
        push16(&mut b, 0x28); // e_machine ARM
        push32(&mut b, 1); // e_version
        push32(&mut b, 0x0800_0000); // e_entry
        push32(&mut b, 52); // e_phoff (right after this header)
        push32(&mut b, 148); // e_shoff (52 + 3 * 32)
        push32(&mut b, 0); // e_flags
        push16(&mut b, 52); // e_ehsize
        push16(&mut b, 32); // e_phentsize
        push16(&mut b, 3); // e_phnum
        push16(&mut b, 40); // e_shentsize
        push16(&mut b, 5); // e_shnum
        push16(&mut b, 4); // e_shstrndx
        assert_eq!(b.len(), 52);

        // Program headers: (type, offset, vaddr, paddr, filesz, memsz, flags, align)
        let phdrs: [(u32, u32, u32, u32, u32, u32, u32); 3] = [
            // .text: flash, R+X
            (1, 0x1000, 0x0800_0000, 0x0800_0000, 0x100, 0x100, 5),
            // .data: VMA in RAM, LMA in flash, RW
            (1, 0x2000, 0x2000_0000, 0x0800_0100, 0x20, 0x20, 6),
            // .bss: RAM, no file bytes, RW
            (1, 0x3000, 0x2000_0020, 0x2000_0020, 0, 0x80, 6),
        ];
        for (t, off, va, pa, fs, ms, fl) in phdrs {
            for x in [t, off, va, pa, fs, ms, fl, 4] {
                push32(&mut b, x);
            }
        }
        assert_eq!(b.len(), 148);

        // Section headers: (name_off, type, flags, addr, offset, size)
        // shstrtab content sits at file offset 348.
        let shdrs: [(u32, u32, u32, u32, u32, u32); 5] = [
            (0, 0, 0, 0, 0, 0),                      // null
            (1, 1, 0x6, 0x0800_0000, 0x1000, 0x100), // .text  PROGBITS ALLOC|EXEC
            (7, 1, 0x3, 0x2000_0000, 0x2000, 0x20),  // .data  PROGBITS WRITE|ALLOC
            (13, 8, 0x3, 0x2000_0020, 0x3000, 0x80), // .bss   NOBITS   WRITE|ALLOC
            (18, 3, 0x0, 0, 348, 28),                // .shstrtab STRTAB
        ];
        for (name, t, fl, addr, off, size) in shdrs {
            for x in [name, t, fl, addr, off, size, 0, 0, 1, 0] {
                push32(&mut b, x);
            }
        }
        assert_eq!(b.len(), 348);
        b.extend_from_slice(b"\0.text\0.data\0.bss\0.shstrtab\0");
        b
    }

    #[test]
    fn elf_totals_and_sections_with_limits() {
        let limits = MemLimits {
            flash: Some(MemRegion {
                origin: 0x0800_0000,
                length: 64 * 1024,
            }),
            ram: Some(MemRegion {
                origin: 0x2000_0000,
                length: 20 * 1024,
            }),
        };
        let u = parse_elf(&cortex_m_style_elf(), limits).unwrap();
        // Flash = .text file bytes + .data initializers; RAM = .data + .bss.
        assert_eq!(u.flash_used, 0x100 + 0x20);
        assert_eq!(u.ram_used, 0x20 + 0x80);
        let by_name = |n: &str| u.sections.iter().find(|s| s.name == n).unwrap();
        assert!(by_name(".text").in_flash && !by_name(".text").in_ram);
        assert!(by_name(".data").in_flash && by_name(".data").in_ram);
        assert!(!by_name(".bss").in_flash && by_name(".bss").in_ram);
        // Non-ALLOC sections (.shstrtab) never appear in the breakdown.
        assert!(u.sections.iter().all(|s| s.name != ".shstrtab"));
    }

    /// Without memory.x (ESP32) the classification falls back to segment
    /// write-flags: same totals for the classic layout.
    #[test]
    fn elf_totals_without_limits_use_write_flags() {
        let u = parse_elf(&cortex_m_style_elf(), MemLimits::default()).unwrap();
        assert_eq!(u.flash_used, 0x100 + 0x20);
        assert_eq!(u.ram_used, 0x20 + 0x80);
        assert!(u.limits.flash.is_none() && u.limits.ram.is_none());
    }

    /// An ELF32 LE with `.vector_table` at the start of flash and `.text` at
    /// `text_addr` - right after it, or past a hole (`_stext` moved).
    fn vectors_then_text_elf(text_addr: u32) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&[0x7f, b'E', b'L', b'F', 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        push16(&mut b, 2); // e_type EXEC
        push16(&mut b, 0x28); // e_machine ARM
        push32(&mut b, 1); // e_version
        push32(&mut b, 0x0800_0000); // e_entry
        push32(&mut b, 52); // e_phoff
        push32(&mut b, 116); // e_shoff (52 + 2 * 32)
        push32(&mut b, 0); // e_flags
        push16(&mut b, 52); // e_ehsize
        push16(&mut b, 32); // e_phentsize
        push16(&mut b, 2); // e_phnum
        push16(&mut b, 40); // e_shentsize
        push16(&mut b, 4); // e_shnum
        push16(&mut b, 3); // e_shstrndx
        for (va, size) in [(0x0800_0000, 0x188), (text_addr, 0x400)] {
            for x in [1, 0x1000, va, va, size, size, 5, 4] {
                push32(&mut b, x);
            }
        }
        assert_eq!(b.len(), 116);
        // shstrtab at file offset 276: "\0.vector_table\0.text\0.shstrtab\0".
        let shdrs: [(u32, u32, u32, u32, u32, u32); 4] = [
            (0, 0, 0, 0, 0, 0),
            (1, 1, 0x2, 0x0800_0000, 0x1000, 0x188),
            (15, 1, 0x6, text_addr, 0x2000, 0x400),
            (21, 3, 0x0, 0, 276, 31),
        ];
        for (name, t, fl, addr, off, size) in shdrs {
            for x in [name, t, fl, addr, off, size, 0, 0, 1, 0] {
                push32(&mut b, x);
            }
        }
        assert_eq!(b.len(), 276);
        b.extend_from_slice(b"\0.vector_table\0.text\0.shstrtab\0");
        b
    }

    /// Found by review: an F2/F4/F7 flash store keeps FLASH whole and moves
    /// `_stext` past it, and the bar still offered the hole to the program -
    /// green at 62% on a 128 KiB F401 whose next bytes would not link.
    #[test]
    fn the_flash_capacity_leaves_out_a_hole_before_the_program() {
        let limits = MemLimits {
            flash: Some(MemRegion {
                origin: 0x0800_0000,
                length: 512 * 1024,
            }),
            ram: None,
        };
        let packed = parse_elf(&vectors_then_text_elf(0x0800_0188), limits).unwrap();
        assert_eq!(packed.limits.flash.unwrap().length, 512 * 1024);
        let store = parse_elf(&vectors_then_text_elf(0x0800_C000), limits).unwrap();
        assert_eq!(
            store.limits.flash.unwrap().length,
            512 * 1024 - (0xC000 - 0x188)
        );
        assert_eq!(store.flash_used, 0x188 + 0x400);
        // Without memory.x there is no capacity to shrink.
        let none = parse_elf(&vectors_then_text_elf(0x0800_C000), MemLimits::default()).unwrap();
        assert!(none.limits.flash.is_none());
    }

    #[test]
    fn garbage_input_errors_instead_of_panicking() {
        assert!(parse_elf(b"not an elf", MemLimits::default()).is_err());
        // Valid magic but truncated right after the ident.
        let mut b = vec![0x7f, b'E', b'L', b'F', 1, 1, 1, 0];
        b.resize(52, 0);
        // phnum = 0 / shnum = 0 → parses to zero usage rather than erroring.
        let u = parse_elf(&b, MemLimits::default()).unwrap();
        assert_eq!((u.flash_used, u.ram_used), (0, 0));
    }
}
