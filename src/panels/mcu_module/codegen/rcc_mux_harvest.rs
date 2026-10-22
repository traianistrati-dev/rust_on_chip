//! The clock mux table [`super::rcc_mux_data`], harvested again from
//! stm32-metapac - an `#[ignore]` test, like the other STM32 tables
//! (`stm32_harvest`), which this one shares its registry lookup with.
//!
//! The Clock tab lets a user pick a source for a peripheral's kernel clock
//! (USART1SEL, I2C1SEL, ADCSEL, ...). Emitting that as embassy config takes
//! three names per selector, and NONE of them is guessable:
//!
//! ```text
//! config.rcc.mux.usart1sel = mux::Usart1sel::HSI;
//!                  ^field         ^enum      ^variant
//! ```
//!
//! The field is not the enum: on WBA, `usart2sel` and `usart3sel` both use
//! `Usartsel`, and `i2c2sel` / `i2c4sel` both use `I2c1sel`.
//!
//! Keyed by RCC VERSION, not by family: a family is not one register block.
//! Across metapac's 1533 chips STM32H7 has four RCC versions, F0 and F3 four
//! each, C0/G0/H5/L0/WL two, and even F4 has two (143 parts on `f4`, six F410s
//! on `f410`). A chip reaches its version by part-number prefix - the SHORTEST
//! prefix that is unambiguous across every chip metapac knows - and
//! `FAMILY_RCC` is the fallback for a part metapac has never heard of, listing
//! only families whose chips all share one version.
//!
//! What a version's chips select comes from the chips themselves: every
//! `kernel_clock: Mux(...)` entry, the list embassy's own build script reads
//! (a name pattern would drop the older families, whose selector is `I2C1SW`),
//! of the peripherals embassy makes singletons, as only those get a
//! `config.rcc.mux` field ([`kernel_muxes`]). Each field's enum is read inside
//! that field's own braces.
//!
//! The first harvester (a Python script) got both wrong. It matched
//! `Field { … enumm: Some(` across field boundaries, so a field with no enum
//! swallowed the next field's - the C0's USART1SEL, the H7's LPUART1SEL and 48
//! more never reached the table (49 of the 50 belong in it) - and it kept 34
//! selectors of peripherals with no register block (DFSDM, SWPMI, ADF, MDF,
//! most of the N6), lines the Clock tab could write and embassy would not
//! compile. Regenerated here, 2026-10-21: 477 selectors, +49 -34, 492.
//!
//! A version's selectors are all of its chips', and a CubeMX clock tree is a
//! whole line's: the STM32L010's has the L0's USART1 mux, though the L010 has
//! no USART1. So the table also says, by part-number prefix, which of its
//! version's selectors a part lacks (`CHIP_LACKS`).
//!
//! Three tests: the table against the crate, byte for byte; every part's
//! lookup against its own metadata ([`every_part_gets_exactly_its_selectors`]);
//! and every selector compiled against embassy-stm32
//! ([`every_mux_selector_compiles_against_embassy`]).
//!
//! ```text
//! cargo test -- --ignored --exact panels::mcu_module::codegen::rcc_mux_harvest::the_rcc_mux_table_matches_metapac --nocapture
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::rcc_mux::MuxField;
use crate::panels::mcu_module::stm32_harvest::{
    cargo_check_crate, check_main, check_or_write, chips, configurations, metapac_or_fail,
};

/// `(field, enum, [(value, variant)])`: one `MuxField`.
type Selector = (String, String, Vec<(u32, String)>);

/// What the table holds, as plain data.
#[derive(Debug, PartialEq, Eq)]
struct Table {
    /// RCC version -> its selectors, by field.
    versions: BTreeMap<String, Vec<Selector>>,
    /// Part-number prefix -> RCC version.
    chip_rcc: BTreeMap<String, String>,
    /// Part-number prefix -> the selectors of its version the part lacks.
    chip_lacks: BTreeMap<String, Vec<String>>,
    /// Family (`stm32l4+`) -> its one RCC version.
    family_rcc: BTreeMap<String, String>,
    /// How many chips the prefixes cover.
    chips: usize,
}

// ── Reading metapac's Rust source ────────────────────────────────────────────

/// The `{ … }` that opens at byte `open`, string literals skipped (a
/// description could hold a brace).
fn braces(text: &str, open: usize) -> &str {
    let bytes = text.as_bytes();
    let (mut depth, mut i, mut in_str) = (0i32, open, false);
    while i < bytes.len() {
        match (in_str, bytes[i]) {
            (true, b'\\') => i += 1,
            (true, b'"') => in_str = false,
            (false, b'"') => in_str = true,
            (false, b'{') => depth += 1,
            (false, b'}') => {
                depth -= 1;
                if depth == 0 {
                    return &text[open..=i];
                }
            }
            _ => {}
        }
        i += 1;
    }
    panic!("an unclosed block at byte {open}")
}

/// Every `<head> { … }` of `text` - `Field`, `Enum`, `EnumVariant` - whole.
fn blocks<'a>(text: &'a str, head: &str) -> Vec<&'a str> {
    let pattern = format!("{head} {{");
    text.match_indices(pattern.as_str())
        .filter(|(at, _)| {
            !text[..*at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        })
        .map(|(at, m)| braces(text, at + m.len() - 1))
        .collect()
}

/// The string after the first `key` of `text`, `key "value"`.
fn quoted<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let at = text.find(key)? + key.len();
    let rest = text[at..].trim_start().strip_prefix('"')?;
    rest.find('"').map(|end| &rest[..end])
}

fn all(s: &str, ok: impl Fn(u8) -> bool) -> bool {
    !s.is_empty() && s.bytes().all(ok)
}

/// Field -> the enum it takes.
type FieldEnums = BTreeMap<String, String>;
/// Enum -> its `(value, variant)`s, in order.
type EnumVariants = BTreeMap<String, Vec<(u32, String)>>;

/// One register file (`registers/rcc_<version>.rs`): field -> its enum (the
/// first field of that name that has one), and enum -> its variants in order.
fn enums_of(text: &str) -> (FieldEnums, EnumVariants) {
    let mut fields = BTreeMap::new();
    for field in blocks(text, "Field") {
        let Some(name) = quoted(field, "name:") else {
            continue;
        };
        let Some(enumm) = field.find("enumm:").map(|at| field[at + 6..].trim_start()) else {
            continue;
        };
        if let Some(e) = enumm.strip_prefix("Some(").and_then(|e| quoted(e, ""))
            && all(name, |c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'
            })
        {
            fields
                .entry(name.to_owned())
                .or_insert_with(|| e.to_owned());
        }
    }
    let mut enums = BTreeMap::new();
    for e in blocks(text, "Enum") {
        let Some(name) = quoted(e, "name:") else {
            continue;
        };
        let variants = blocks(e, "EnumVariant")
            .into_iter()
            .filter_map(|v| {
                let name = quoted(v, "name:")?;
                let at = v.find("value:")? + 6;
                let value = v[at..].trim_start();
                let value: u32 = value[..value.find(|c: char| !c.is_ascii_digit())?]
                    .parse()
                    .ok()?;
                Some((value, name.to_owned()))
            })
            .collect();
        enums.insert(name.to_owned(), variants);
    }
    (fields, enums)
}

/// The RCC block's version in a shared `metadata_NNNN.rs`.
fn rcc_version(body: &str) -> Option<&str> {
    let rest = &body[body.find("name: \"RCC\",")?..];
    rest.match_indices("kind: \"rcc\",").find_map(|(at, m)| {
        let v = quoted(
            rest[at + m.len()..].trim_start().strip_prefix("version:")?,
            "",
        )?;
        all(v, |c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'
        })
        .then_some(v)
    })
}

/// The selectors a shared `metadata_NNNN.rs`'s chips use, lower-cased: the
/// `kernel_clock: Mux(PeripheralRccRegister { register, field })` of every
/// peripheral embassy-stm32 makes a singleton.
///
/// Only those reach `config.rcc.mux`: embassy builds `ClockMux` from its
/// singletons, and a peripheral is one when metapac gives it a register block
/// (a GPIO port and EXTI aside - its build script, `singletons`). A DFSDM,
/// SWPMI, ADF or MDF with no register block in metapac 21 has a selector in
/// the RCC and none in embassy, so a line naming it would not compile.
fn kernel_muxes(body: &str) -> BTreeSet<String> {
    blocks(body, "Peripheral")
        .into_iter()
        .filter(|p| singleton(p))
        .filter_map(kernel_mux)
        .collect()
}

/// Is this `Peripheral { … }` a singleton in embassy-stm32?
fn singleton(peripheral: &str) -> bool {
    let Some(at) = peripheral.find("registers:") else {
        return false;
    };
    let registers = peripheral[at + "registers:".len()..].trim_start();
    registers.starts_with("Some(") && !matches!(quoted(registers, "kind:"), Some("gpio" | "exti"))
}

/// The selector field this peripheral's kernel clock goes through, if a mux.
fn kernel_mux(peripheral: &str) -> Option<String> {
    let upper = |s: &str| {
        all(s, |c| {
            c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_'
        })
    };
    let head = "kernel_clock: Mux(PeripheralRccRegister {";
    let rest = peripheral[peripheral.find(head)? + head.len()..]
        .trim_start()
        .strip_prefix("register: \"")?;
    let end = rest.find('"')?;
    if !upper(&rest[..end]) {
        return None;
    }
    let field = rest[end + 1..]
        .strip_prefix(',')?
        .trim_start()
        .strip_prefix("field:")?;
    let field = quoted(field, "")?;
    upper(field).then(|| field.to_ascii_lowercase())
}

// ── The harvest ──────────────────────────────────────────────────────────────

/// Shortest part-number prefix that pins a chip's RCC version, then without
/// the rules a shorter one already implies.
///
/// `keys` is chip -> what the rule answers: its RCC version for `CHIP_RCC`,
/// the selectors it lacks for `CHIP_LACKS`.
fn prefix_rules<K: Ord + Clone>(keys: &BTreeMap<String, K>) -> BTreeMap<String, K> {
    // In the order chips find them, as a shorter rule is judged first below.
    let mut found: Vec<(String, K)> = Vec::new();
    for (chip, key) in keys {
        for n in 6..=chip.len() {
            let pre = &chip[..n];
            let distinct: BTreeSet<&K> = keys
                .iter()
                .filter(|(c, _)| c.starts_with(pre))
                .map(|(_, k)| k)
                .collect();
            if distinct.len() == 1 {
                if !found.iter().any(|(p, _)| p == pre) {
                    found.push((pre.to_owned(), key.clone()));
                }
                break;
            }
        }
    }
    found.sort_by_key(|(p, _)| p.len()); // stable: ties keep their order
    let mut rules: BTreeMap<String, K> = BTreeMap::new();
    for (pre, key) in found {
        if !rules
            .iter()
            .any(|(p, k)| pre.starts_with(p.as_str()) && *k == key)
        {
            rules.insert(pre, key);
        }
    }
    rules
}

/// What one chip's metadata says about its clock selectors.
struct ChipMuxes {
    /// Its RCC register version (`c0`, `h7rm0433`).
    version: String,
    /// Its family as the importer keys it: `stm32l4+`.
    family: String,
    /// The selectors its peripherals' kernel clocks go through, lower-cased.
    fields: BTreeSet<String>,
    /// Single or dual bank is an option-byte choice: embassy-stm32 then needs
    /// `single-bank` (or `dual-bank`), or its build script panics.
    bank_choice: bool,
}

/// Every chip folder of `metapac` that has an RCC block, and its selectors.
fn chip_muxes(metapac: &Path) -> BTreeMap<String, ChipMuxes> {
    let chips_dir = metapac.join("src").join("chips");
    let mut out = BTreeMap::new();
    let mut shared: BTreeMap<String, (Option<String>, BTreeSet<String>)> = BTreeMap::new();
    for (chip, head) in chips(metapac) {
        if !chip.starts_with("stm32") {
            continue;
        }
        let Some(include) = quoted(&head, "include!(").and_then(|p| p.strip_prefix("../")) else {
            continue;
        };
        let Some(family) = quoted(&head, "family:") else {
            continue;
        };
        let (version, fields) = shared
            .entry(include.to_owned())
            .or_insert_with(|| {
                let body = std::fs::read_to_string(chips_dir.join(include)).unwrap_or_default();
                (rcc_version(&body).map(str::to_owned), kernel_muxes(&body))
            })
            .clone();
        let Some(version) = version else { continue };
        let bank_choice = configurations(&head).len() > 1;
        out.insert(
            chip,
            ChipMuxes {
                version,
                family: family.to_ascii_lowercase(),
                fields,
                bank_choice,
            },
        );
    }
    out
}

/// The table stm32-metapac says, from `metapac`'s `src/chips` and `src/registers`.
fn harvest(metapac: &Path) -> Table {
    let chips = chip_muxes(metapac);
    // chip folder -> (RCC version, family)
    let mut chip_rcc: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut muxes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (chip, c) in &chips {
        muxes
            .entry(c.version.clone())
            .or_default()
            .extend(c.fields.iter().cloned());
        chip_rcc.insert(chip.clone(), (c.version.clone(), c.family.clone()));
    }

    let mut versions = BTreeMap::new();
    for (version, want) in &muxes {
        let path = metapac
            .join("src")
            .join("registers")
            .join(format!("rcc_{version}.rs"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (fields, enums) = enums_of(&text);
        let found: Vec<Selector> = want
            .iter()
            .filter_map(|field| {
                let enum_name = fields.get(field)?;
                // DISABLE is the "off" state, not a source: embassy's
                // generator skips it too.
                let variants: Vec<(u32, String)> = enums
                    .get(enum_name)
                    .into_iter()
                    .flatten()
                    .filter(|(_, n)| n != "DISABLE")
                    .cloned()
                    .collect();
                (!variants.is_empty()).then(|| (field.clone(), enum_name.clone(), variants))
            })
            .collect();
        if !found.is_empty() {
            versions.insert(version.clone(), found);
        }
    }

    let mut by_family: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (version, family) in chip_rcc.values() {
        by_family.entry(family).or_default().insert(version);
    }
    let family_rcc = by_family
        .into_iter()
        .filter(|(_, v)| v.len() == 1)
        .map(|(f, v)| {
            (
                f.to_owned(),
                v.into_iter().next().unwrap_or_default().to_owned(),
            )
        })
        .collect();

    // Per part, the selectors of its version it does not have. A version's
    // table is every one of its chips' selectors, and a CubeMX clock tree is
    // a whole line's: an STM32L010's tree has the L0's USART1 mux, the L010
    // has no USART1, and embassy gives it no `usart1sel`.
    let lacks: BTreeMap<String, Vec<String>> = chips
        .iter()
        .map(|(chip, c)| {
            let missing = versions
                .get(&c.version)
                .into_iter()
                .flatten()
                .map(|s: &Selector| &s.0)
                .filter(|f| !c.fields.contains(*f))
                .cloned()
                .collect();
            (chip.clone(), missing)
        })
        .collect();
    let version_of: BTreeMap<String, String> = chip_rcc
        .iter()
        .map(|(chip, (version, _))| (chip.clone(), version.clone()))
        .collect();
    Table {
        versions,
        chip_rcc: prefix_rules(&version_of),
        // No entry is the common case: a part with everything its version has.
        chip_lacks: prefix_rules(&lacks)
            .into_iter()
            .filter(|(_, l)| !l.is_empty())
            .collect(),
        family_rcc,
        chips: chip_rcc.len(),
    }
}

// ── The file ─────────────────────────────────────────────────────────────────

/// `rcc_mux_data.rs` for `table`, one `MuxField` per line - the layout
/// rustfmt then turns into the committed one ([`rustfmt`]).
fn write(table: &Table, source: &str) -> String {
    let static_name = |v: &str| format!("V_{}", v.to_ascii_uppercase().replace('-', "_"));
    let mut o = String::from(concat!(
        "//! Per-peripheral clock mux tables, harvested from stm32-metapac.\n",
        "//!\n",
        "//! GENERATED by `cargo test` (`rcc_mux_harvest`, `EIDE_REGEN=1`) - do not\n",
        "//! edit by hand.\n",
    ));
    let _ = writeln!(o, "//! Source: {source}");
    o.push_str(concat!(
        "//!\n",
        "//! Keyed by RCC register VERSION, because a family is not one register\n",
        "//! block: STM32H7 has four, F0 and F3 four each, and even F4 has two\n",
        "//! (`f4`, and `f410` for six parts). A chip reaches its version through\n",
        "//! [`CHIP_RCC`] by part-number prefix; [`FAMILY_RCC`] is the fallback\n",
        "//! for a part metapac does not know, and only lists families whose\n",
        "//! chips all share one version.\n",
        "\n",
        "use super::rcc_mux::MuxField;\n",
        "\n",
    ));
    for (version, fields) in &table.versions {
        let _ = writeln!(o, "/// `rcc_{version}` — {} selectors.", fields.len());
        let _ = writeln!(o, "static {}: &[MuxField] = &[", static_name(version));
        for (field, enum_name, variants) in fields {
            let vs: Vec<String> = variants
                .iter()
                .map(|(v, n)| format!("({v}, \"{n}\")"))
                .collect();
            let _ = writeln!(
                o,
                "    MuxField {{ field: \"{field}\", enum_name: \"{enum_name}\", variants: &[{}] }},",
                vs.join(", ")
            );
        }
        o.push_str("];\n\n");
    }
    o.push_str("/// Every RCC version that exposes at least one selector.\n");
    o.push_str("pub static VERSIONS: &[(&str, &[MuxField])] = &[\n");
    for version in table.versions.keys() {
        let _ = writeln!(o, "    (\"{version}\", {}),", static_name(version));
    }
    o.push_str("];\n\n");
    o.push_str("/// Part-number prefix -> RCC version. Longest match wins, so the\n");
    let _ = writeln!(
        o,
        "/// order here does not matter; {} rules cover {} chips.",
        table.chip_rcc.len(),
        table.chips
    );
    o.push_str("pub static CHIP_RCC: &[(&str, &str)] = &[\n");
    for (pre, version) in &table.chip_rcc {
        let _ = writeln!(o, "    (\"{pre}\", \"{version}\"),");
    }
    o.push_str(concat!(
        "];\n",
        "\n",
        "/// Part-number prefix -> the selectors of its RCC version the part does\n",
        "/// not have: a version's table is all of its chips', and a CubeMX clock\n",
        "/// tree is a whole line's - the STM32L010's has the L0's USART1 mux, and\n",
        "/// embassy gives the L010 no `usart1sel`. Longest match wins; a part with\n",
        "/// no entry has every selector of its version.\n",
        "pub static CHIP_LACKS: &[(&str, &[&str])] = &[\n",
    ));
    for (pre, fields) in &table.chip_lacks {
        let fields: Vec<String> = fields.iter().map(|f| format!("\"{f}\"")).collect();
        let _ = writeln!(o, "    (\"{pre}\", &[{}]),", fields.join(", "));
    }
    o.push_str(concat!(
        "];\n",
        "\n",
        "/// Families whose chips all share one RCC version — the fallback\n",
        "/// when a part number matches no prefix (a chip newer than this\n",
        "/// harvest). Ambiguous families are absent ON PURPOSE.\n",
        "pub static FAMILY_RCC: &[(&str, &str)] = &[\n",
    ));
    for (family, version) in &table.family_rcc {
        let _ = writeln!(o, "    (\"{family}\", \"{version}\"),");
    }
    o.push_str("];\n");
    o
}

/// `text` through rustfmt (edition 2024, the crate's), as `cargo fmt` would
/// leave the file.
fn rustfmt(text: &str) -> String {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new("rustfmt")
        .args(["--edition", "2024"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("rustfmt on PATH");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(text.as_bytes())
        .expect("rustfmt reads");
    let out = child.wait_with_output().expect("rustfmt runs");
    assert!(
        out.status.success(),
        "rustfmt: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("UTF-8")
        .replace("\r\n", "\n")
}

// ── The test ─────────────────────────────────────────────────────────────────

const PATH: &str = "src/panels/mcu_module/codegen/rcc_mux_data.rs";

/// `rcc_mux_data.rs` against stm32-metapac 21, byte for byte as `cargo fmt`
/// leaves it; with `EIDE_REGEN=1` it writes the harvest.
#[test]
#[ignore]
fn the_rcc_mux_table_matches_metapac() {
    let metapac = metapac_or_fail();
    let source = metapac
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned();
    check_or_write(PATH, &rustfmt(&write(&harvest(&metapac), &source)));
}

/// What the IDE's lookup gives each of metapac's 1533 parts - the table's
/// routing by prefix ([`super::rcc_mux::fields_for_chip`]: `CHIP_RCC`, then
/// `CHIP_LACKS`) - is exactly the part's own: its version's selectors that its
/// peripherals use. The prefixes are the shortest that hold across every
/// chip, so this is the check that they still do.
#[test]
#[ignore]
fn every_part_gets_exactly_its_selectors() {
    let mut wrong = Vec::new();
    let chips = chip_muxes(&metapac_or_fail());
    for (chip, c) in &chips {
        let table = super::rcc_mux_data::VERSIONS
            .iter()
            .find(|(v, _)| *v == c.version)
            .map(|(_, f)| *f)
            .unwrap_or(&[]);
        let want: BTreeSet<&str> = table
            .iter()
            .map(|f| f.field)
            .filter(|f| c.fields.contains(*f))
            .collect();
        let got: BTreeSet<&str> = super::rcc_mux::fields_for_chip(chip, &c.family)
            .iter()
            .map(|f| f.field)
            .collect();
        if got != want {
            wrong.push(format!(
                "{chip}: gets {:?}, has not {:?}",
                got.difference(&want).collect::<Vec<_>>(),
                want.difference(&got).collect::<Vec<_>>()
            ));
        }
    }
    println!("{} parts routed", chips.len());
    assert!(
        wrong.is_empty(),
        "{} parts:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// The target a part of `family` (`stm32wl`) builds for: its core's, as
/// `stm32_pin_data::target_for` picks it - the WL's Cortex-M4 has no FPU.
fn target_of(family: &str) -> &'static str {
    match family {
        "stm32c0" | "stm32f0" | "stm32g0" | "stm32l0" | "stm32u0" | "stm32wb0" => {
            "thumbv6m-none-eabi"
        }
        "stm32f1" | "stm32f2" | "stm32l1" => "thumbv7m-none-eabi",
        "stm32wl" => "thumbv7em-none-eabi",
        "stm32f3" | "stm32f4" | "stm32f7" | "stm32g4" | "stm32h7" | "stm32l4" | "stm32l4+"
        | "stm32wb" => "thumbv7em-none-eabihf",
        "stm32h5" | "stm32l5" | "stm32n6" | "stm32u3" | "stm32u5" | "stm32wba" => {
            "thumbv8m.main-none-eabihf"
        }
        other => panic!("{other}: no core known for this family"),
    }
}

/// Every selector the table lists, compiled against embassy-stm32: each of
/// its variants assigned exactly as the Clock tab writes it
/// (`rcc_mux::emit_lines`), on a part whose peripherals use that selector.
///
/// The table is metapac's, and embassy-stm32 builds its `config.rcc.mux` from
/// metapac too - but only from the peripherals the chip has, and through its
/// own reading. A line the IDE writes that embassy does not have is a project
/// that does not build, so the compiler has the last word. Per RCC version,
/// the fewest published parts (application cores) that between them use
/// every field; a version with no published part (WB0) is reported, not
/// compiled - the import refuses those parts.
///
/// `EIDE_MUX_CHECK=<dir>`: where the crates build, one per part, sharing
/// `<dir>\target` (a few GB: off the repo and off the matrix drive).
/// `EIDE_MUX_LOCK=<Cargo.lock>` seeds every crate with a known-good lock file.
#[test]
#[ignore]
fn every_mux_selector_compiles_against_embassy() {
    use crate::panels::mcu_module::stm32_pin_data::embassy_publishes;
    let work = std::env::var_os("EIDE_MUX_CHECK")
        .map(PathBuf::from)
        .expect("EIDE_MUX_CHECK=<folder>: where the check crates build (a few GB, off the repo)");
    let lock = std::env::var_os("EIDE_MUX_LOCK").map(PathBuf::from);
    let chips = chip_muxes(&metapac_or_fail());

    // Which parts, and which of the table's selectors on each.
    let mut plan: Vec<(&str, &ChipMuxes, Vec<&MuxField>)> = Vec::new();
    let mut unreached = Vec::new();
    for (version, fields) in super::rcc_mux_data::VERSIONS {
        let candidates: Vec<(&String, &ChipMuxes)> = chips
            .iter()
            .filter(|(chip, c)| {
                // A dual-core part's application core only, the one the IDE
                // builds for: the H7's M7, the WL5's M4.
                let second_core = (chip.starts_with("stm32h7") && chip.ends_with("-cm4"))
                    || chip.ends_with("-cm0p");
                c.version == *version && embassy_publishes(chip) && !second_core
            })
            .collect();
        if candidates.is_empty() {
            println!("{version}: no part embassy-stm32 publishes, nothing to compile");
            continue;
        }
        let mut left: BTreeSet<&str> = fields.iter().map(|f| f.field).collect();
        while !left.is_empty() {
            // The part that uses the most of what is left; by name on a tie.
            let Some((chip, c)) = candidates.iter().copied().max_by_key(|(chip, c)| {
                let uses = left.iter().filter(|f| c.fields.contains(**f)).count();
                (uses, std::cmp::Reverse(chip.as_str()))
            }) else {
                break;
            };
            let here: Vec<&MuxField> = fields
                .iter()
                .filter(|f| left.contains(&f.field) && c.fields.contains(f.field))
                .collect();
            if here.is_empty() {
                break;
            }
            for f in &here {
                left.remove(&f.field);
            }
            plan.push((chip.as_str(), c, here));
        }
        unreached.extend(
            left.iter()
                .map(|f| format!("{version}/{f}: no published part uses it")),
        );
    }

    let mut failed = Vec::new();
    for (chip, c, fields) in &plan {
        let mut body = String::from(concat!(
            "#[allow(unused)]\n",
            "fn every_selector() {\n",
            "    let mut config = embassy_stm32::Config::default();\n",
            "    {\n",
            "        use embassy_stm32::rcc;\n",
        ));
        // The lines `emit_lines` starts the LSE with: every family but the
        // N6, whose `Config` has no `ls`.
        if c.family != "stm32n6" {
            use super::rcc_mux::{LSE_ON, LSI_ON, RTC_ON_LSI};
            for line in [LSE_ON, RTC_ON_LSI, LSI_ON] {
                let _ = writeln!(body, "{line}");
            }
        }
        let mut lines = 0;
        for f in fields {
            for (_, variant) in f.variants {
                let _ = writeln!(
                    body,
                    "        config.rcc.mux.{} = rcc::mux::{}::{variant};",
                    f.field, f.enum_name
                );
                lines += 1;
            }
        }
        body.push_str("    }\n}\n");
        let mut features = vec![format!("\"{chip}\""), "\"time-driver-any\"".to_owned()];
        if c.bank_choice {
            features.push("\"single-bank\"".to_owned());
        }
        let dependency = format!(
            "embassy-stm32 = {{ version = \"0.6\", features = [{}] }}",
            features.join(", ")
        );
        let result = cargo_check_crate(
            &work,
            chip,
            &dependency,
            target_of(&c.family),
            &check_main(&body),
            lock.as_deref(),
        );
        println!(
            "{} {chip} ({}: {} selectors, {lines} lines)",
            if result.is_ok() { "OK  " } else { "FAIL" },
            c.version,
            fields.len()
        );
        if let Err(errors) = result {
            for e in errors.iter().take(12) {
                println!("     {e}");
            }
            failed.push(format!(
                "{chip}: {}",
                errors.first().map_or("(no error line)", String::as_str)
            ));
        }
    }
    println!(
        "\n{}/{} parts check clean",
        plan.len() - failed.len(),
        plan.len()
    );
    failed.extend(unreached);
    assert!(
        failed.is_empty(),
        "{} failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

/// The reader on text shaped like metapac's: a field's enum is read inside
/// its own braces, never the next field's.
#[test]
fn a_field_without_an_enum_does_not_take_the_next_ones() {
    let regs = r#"
        Field {
            name: "adc1smen",
            description: Some(
                "ADC clock {enable} during Sleep mode",
            ),
            bit_offset: BitOffset::Regular(
                RegularBitOffset {
                    offset: 20,
                },
            ),
            bit_size: 1,
            array: None,
            enumm: None,
        },
        Field {
            name: "usart1sel",
            description: None,
            bit_offset: BitOffset::Regular(
                RegularBitOffset {
                    offset: 0,
                },
            ),
            bit_size: 2,
            array: None,
            enumm: Some(
                "Usart1sel",
            ),
        },
        Enum {
            name: "Usart1sel",
            description: None,
            bit_size: 2,
            variants: &[
                EnumVariant {
                    name: "PCLK1",
                    description: Some(
                        "PCLK1 selected",
                    ),
                    value: 0,
                },
                EnumVariant {
                    name: "DISABLE",
                    description: None,
                    value: 3,
                },
            ],
        },
    "#;
    let (fields, enums) = enums_of(regs);
    assert_eq!(
        fields.get("usart1sel").map(String::as_str),
        Some("Usart1sel")
    );
    assert_eq!(
        fields.get("adc1smen"),
        None,
        "the regex's bug: it took Usart1sel"
    );
    assert_eq!(
        enums["Usart1sel"],
        [(0, "PCLK1".to_owned()), (3, "DISABLE".to_owned())]
    );

    let meta = r#"
    peripherals: &[
        Peripheral {
            name: "RCC",
            address: 1073876992,
            registers: Some(PeripheralRegisters {
                kind: "rcc",
                version: "c0",
                block: "RCC",
                ir: &rcc::REGISTERS,
            }),
            rcc: None,
        },
        Peripheral {
            name: "USART1",
            address: 0x40013800,
            registers: Some(PeripheralRegisters {
                kind: "usart",
                version: "v4",
                block: "USART",
                ir: &usart::REGISTERS,
            }),
            rcc: Some(PeripheralRcc {
                bus_clock: "PCLK2",
                kernel_clock: Mux(PeripheralRccRegister {
                    register: "CCIPR",
                    field: "USART1SEL",
                }),
            }),
        },
        Peripheral {
            name: "DFSDM1",
            address: 0x40016000,
            registers: None,
            rcc: Some(PeripheralRcc {
                bus_clock: "PCLK2",
                kernel_clock: Mux(PeripheralRccRegister {
                    register: "CCIPR2",
                    field: "DFSDMSEL",
                }),
            }),
        },
    ],
    "#;
    assert_eq!(rcc_version(meta), Some("c0"));
    // DFSDM1 has no register block, so no singleton and no `ClockMux` field.
    assert_eq!(kernel_muxes(meta), BTreeSet::from(["usart1sel".to_owned()]));

    // The shortest unambiguous prefix, without the ones a shorter implies.
    let chips: BTreeMap<String, String> = [
        ("stm32f401cc", "f4"),
        ("stm32f410t8", "f410"),
        ("stm32f411re", "f4"),
        ("stm32g431cb", "g4"),
    ]
    .into_iter()
    .map(|(c, v)| (c.to_owned(), v.to_owned()))
    .collect();
    let rules = prefix_rules(&chips);
    assert_eq!(
        rules.into_iter().collect::<Vec<_>>(),
        [
            ("stm32f40".to_owned(), "f4".to_owned()),
            ("stm32f410".to_owned(), "f410".to_owned()),
            ("stm32f411".to_owned(), "f4".to_owned()),
            ("stm32g".to_owned(), "g4".to_owned()),
        ]
    );
}
