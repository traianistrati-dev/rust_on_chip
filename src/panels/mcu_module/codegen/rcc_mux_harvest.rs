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
//! (a name pattern would drop the older families, whose selector is `I2C1SW`).
//! Each field's enum is read inside that field's own braces. The first
//! harvester (a Python script) matched `Field { … enumm: Some(` across field
//! boundaries, so a field with no enum swallowed the next field's: the C0's
//! USART1SEL, the H7's LPUART1SEL and 48 more never reached the table. The
//! check below pins that gap until the table is regenerated.
//!
//! ```text
//! cargo test -- --ignored --exact panels::mcu_module::codegen::rcc_mux_harvest::the_rcc_mux_table_matches_metapac --nocapture
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use crate::panels::mcu_module::stm32_harvest::{chips, metapac_or_fail, regen};

/// `(field, enum, [(value, variant)])`: one `MuxField`.
type Selector = (String, String, Vec<(u32, String)>);

/// What the table holds, as plain data.
#[derive(Debug, PartialEq, Eq)]
struct Table {
    /// RCC version -> its selectors, by field.
    versions: BTreeMap<String, Vec<Selector>>,
    /// Part-number prefix -> RCC version.
    chip_rcc: BTreeMap<String, String>,
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

/// Every `kernel_clock: Mux(PeripheralRccRegister { register, field })` of a
/// shared `metadata_NNNN.rs`: the selectors its chips use, lower-cased.
fn kernel_muxes(body: &str) -> BTreeSet<String> {
    let upper = |s: &str| {
        all(s, |c| {
            c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_'
        })
    };
    body.match_indices("kernel_clock: Mux(PeripheralRccRegister {")
        .filter_map(|(at, m)| {
            let rest = body[at + m.len()..]
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
        })
        .collect()
}

// ── The harvest ──────────────────────────────────────────────────────────────

/// Shortest part-number prefix that pins a chip's RCC version, then without
/// the rules a shorter one already implies.
fn prefix_rules(chips: &BTreeMap<String, (String, String)>) -> BTreeMap<String, String> {
    // In the order chips find them, as a shorter rule is judged first below.
    let mut found: Vec<(String, String)> = Vec::new();
    for (chip, (version, _)) in chips {
        for n in 6..=chip.len() {
            let pre = &chip[..n];
            let versions: BTreeSet<&str> = chips
                .iter()
                .filter(|(c, _)| c.starts_with(pre))
                .map(|(_, (v, _))| v.as_str())
                .collect();
            if versions.len() == 1 {
                if !found.iter().any(|(p, _)| p == pre) {
                    found.push((pre.to_owned(), version.clone()));
                }
                break;
            }
        }
    }
    found.sort_by_key(|(p, _)| p.len()); // stable: ties keep their order
    let mut rules: BTreeMap<String, String> = BTreeMap::new();
    for (pre, version) in found {
        if !rules
            .iter()
            .any(|(p, v)| pre.starts_with(p.as_str()) && *v == version)
        {
            rules.insert(pre, version);
        }
    }
    rules
}

/// The table stm32-metapac says, from `metapac`'s `src/chips` and `src/registers`.
fn harvest(metapac: &Path) -> Table {
    let chips_dir = metapac.join("src").join("chips");
    // chip folder -> (RCC version, family as the importer keys it: `stm32l4+`)
    let mut chip_rcc: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut muxes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
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
        chip_rcc.insert(chip, (version.clone(), family.to_ascii_lowercase()));
        muxes.entry(version).or_default().extend(fields);
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
    Table {
        versions,
        chip_rcc: prefix_rules(&chip_rcc),
        family_rcc,
        chips: chip_rcc.len(),
    }
}

/// The table compiled into the IDE, as the same data.
fn committed() -> Table {
    let data = super::rcc_mux_data::VERSIONS;
    Table {
        versions: data
            .iter()
            .map(|(v, fields)| {
                let fields = fields
                    .iter()
                    .map(|f| {
                        let variants = f
                            .variants
                            .iter()
                            .map(|(n, s)| (*n, (*s).to_owned()))
                            .collect();
                        (f.field.to_owned(), f.enum_name.to_owned(), variants)
                    })
                    .collect();
                ((*v).to_owned(), fields)
            })
            .collect(),
        chip_rcc: super::rcc_mux_data::CHIP_RCC
            .iter()
            .map(|(p, v)| ((*p).to_owned(), (*v).to_owned()))
            .collect(),
        family_rcc: super::rcc_mux_data::FAMILY_RCC
            .iter()
            .map(|(f, v)| ((*f).to_owned(), (*v).to_owned()))
            .collect(),
        // The doc line above CHIP_RCC says it; the data does not carry it.
        chips: 0,
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
        "//! GENERATED - do not edit by hand. `codegen::rcc_mux_harvest` checks it\n",
        "//! against the crate and, with `EIDE_REGEN=1`, rewrites it.\n",
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

/// The selectors metapac lists that the committed table lacks: the first
/// harvester's regex dropped them (see the module doc). The table is NOT the
/// harvest until a regeneration adds them - which changes what the Clock tab
/// emits on those parts, so it is a change of its own: after it, set this to
/// 0 and move the selector count `the_harvested_tables_are_sane` pins in
/// `rcc_mux.rs` (477) to the new total.
const KNOWN_GAP: usize = 50;

/// `rcc_mux_data.rs` against stm32-metapac 21.
///
/// Three things: the writer gives the committed file back from the
/// committed data (so a regeneration changes only what the crate changed);
/// every committed selector, prefix rule and family rule is what metapac
/// says; and what metapac lists beyond the table is exactly the known gap.
/// With `EIDE_REGEN=1` it writes the whole harvest instead - the gap included.
#[test]
#[ignore]
fn the_rcc_mux_table_matches_metapac() {
    let metapac = metapac_or_fail();
    let source = metapac
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(PATH);
    let mut harvested = harvest(&metapac);
    if regen() {
        std::fs::write(&path, rustfmt(&write(&harvested, &source))).expect("writable");
        println!("wrote {PATH}");
        return;
    }

    // The writer, on the committed data: the committed file, byte for byte.
    let file = std::fs::read_to_string(&path)
        .expect("readable")
        .replace("\r\n", "\n");
    let mut table = committed();
    table.chips = harvested.chips;
    let rewritten = rustfmt(&write(&table, &source));
    if rewritten != file {
        // Where they part - or, one a prefix of the other, where the shorter ends.
        let (old, new): (Vec<&str>, Vec<&str>) =
            (file.lines().collect(), rewritten.lines().collect());
        let at = old
            .iter()
            .zip(&new)
            .position(|(a, b)| a != b)
            .unwrap_or(old.len().min(new.len()));
        panic!(
            "the writer does not give {PATH} back, from line {} ({} lines in the file, {} written):\n  file:   {:?}\n  writer: {:?}",
            at + 1,
            old.len(),
            new.len(),
            old.get(at),
            new.get(at)
        );
    }

    // The routing: every rule as metapac says it.
    assert_eq!(
        table.chip_rcc, harvested.chip_rcc,
        "CHIP_RCC moved: regenerate"
    );
    assert_eq!(
        table.family_rcc, harvested.family_rcc,
        "FAMILY_RCC moved: regenerate"
    );

    // Every committed selector is metapac's, field, enum and variants.
    let mut wrong = Vec::new();
    for (version, fields) in &table.versions {
        let theirs = harvested
            .versions
            .get(version)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for f in fields {
            if !theirs.contains(f) {
                wrong.push(format!(
                    "{version}: {f:?}, metapac: {:?}",
                    theirs.iter().find(|t| t.0 == f.0)
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} committed selectors metapac does not say:\n{}",
        wrong.len(),
        wrong.join("\n")
    );

    // And beyond it, exactly the known gap.
    let mut missing = Vec::new();
    for (version, fields) in &mut harvested.versions {
        let ours = table
            .versions
            .get(version)
            .map(Vec::as_slice)
            .unwrap_or_default();
        fields.retain(|f| !ours.contains(f));
        missing.extend(fields.iter().map(|f| format!("{version}/{}", f.0)));
    }
    println!(
        "{} selectors metapac lists beyond the table:\n{}",
        missing.len(),
        missing.join(", ")
    );
    assert_eq!(
        missing.len(),
        KNOWN_GAP,
        "the table's gap moved: see KNOWN_GAP for what a regeneration takes"
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
            name: "RCC",
            address: 1073876992,
            registers: Some(
                PeripheralRegisters {
                    kind: "rcc",
                    version: "c0",
                    block: "RCC",
                },
            ),
            kernel_clock: Mux(PeripheralRccRegister {
                register: "CCIPR",
                field: "USART1SEL",
            }),
    "#;
    assert_eq!(rcc_version(meta), Some("c0"));
    assert_eq!(kernel_muxes(meta), BTreeSet::from(["usart1sel".to_owned()]));

    // The shortest unambiguous prefix, without the ones a shorter implies.
    let chips: BTreeMap<String, (String, String)> = [
        ("stm32f401cc", "f4"),
        ("stm32f410t8", "f410"),
        ("stm32f411re", "f4"),
        ("stm32g431cb", "g4"),
    ]
    .into_iter()
    .map(|(c, v)| (c.to_owned(), (v.to_owned(), "x".to_owned())))
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
