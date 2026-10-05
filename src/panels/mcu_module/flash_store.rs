//! The Configuration tab's flash store: a region of the chip's own NOR flash
//! that the firmware keeps settings in, and what reserves it.
//!
//! The firmware side is `sequential-storage` over `esp-storage` (see
//! `codegen::flash_store_gen`); this module holds the facts and the checks the
//! tab and the generators share. Every fact below was MEASURED (espflash 4.4.0,
//! esp-bootloader-esp-idf 0.5.0, esp-storage 0.9.0, sequential-storage 8.0.2),
//! because three of them contradict what the obvious design assumes:
//!
//! - The store's row is `data, undefined`. espflash panics on a numeric data
//!   subtype it has no name for (`0x99`, `0x40`, `0xfe` - inside
//!   esp-idf-part 0.6's `unwrap`), and esp-bootloader-esp-idf's
//!   `partition_type()` unwraps the subtype as well. `undefined` (6) is the one
//!   subtype both accept, and the generated lookup is raw anyway.
//! - Without a table of our own NOTHING reserves the top of flash: espflash's
//!   default table runs `factory` to the end of the chip (`0x10000..0x400000`
//!   on 4 MB), so a store up there sits inside the app partition.
//! - espflash checks only the SUM of the partition sizes against the flash,
//!   never `offset + size`, so a row past the end of the chip passes every
//!   host-side espflash check and fails only at runtime. That check is ours.
//!
//! The alternative with no table at all is the default table's own `nvs`
//! partition ([`NVS_RANGE`]): reserved by every espflash table, and never
//! erased by `espflash flash`, which erases only the sectors it writes.

use std::ops::Range;

/// The flash's erase unit on every ESP part (`esp_storage::FlashStorage::SECTOR_SIZE`),
/// and the alignment a `data` partition needs.
pub const SECTOR: u32 = 0x1000;
/// `sequential-storage` keeps one page as the migration buffer, so a map needs
/// at least two (`MapConfig::new` panics below that).
pub const MIN_SECTORS: u32 = 2;
/// Where espflash puts the app partition, and the alignment an app needs.
pub const APP_OFFSET: u32 = 0x10000;
/// The smallest app partition the card lets the store squeeze down to.
pub const MIN_APP: u32 = 0x10000;
/// The default table's `nvs` partition - the same on every espflash table and
/// every ESP chip: `nvs, data, nvs, 0x9000, 0x6000`.
pub const NVS_RANGE: Range<u32> = 0x9000..0xF000;
/// ESP-IDF partition type `data`.
pub const TYPE_DATA: u8 = 0x01;
/// ESP-IDF `data` subtypes this module writes.
pub const SUBTYPE_NVS: u8 = 0x02;
pub const SUBTYPE_UNDEFINED: u8 = 0x06;
/// The store's row name in partitions.csv. At most 16 characters: espflash
/// truncates a longer label without a word.
pub const LABEL: &str = "flash_store";
/// The flash sizes the card offers.
pub const FLASH_SIZES: [u32; 4] = [0x20_0000, 0x40_0000, 0x80_0000, 0x100_0000];

/// The families the store is generated for. ESP32-C3 first: it is the chip the
/// feature was compiled and run against. The others have the FLASH peripheral
/// and an esp-storage feature, but two need more than this generator writes -
/// the ESP32 and S3 must park their second core before a write, and the ESP32
/// refuses the Debug build's `opt-level = 1` in esp-storage's build script.
pub const SUPPORTED: &[&str] = &["esp32c3"];

/// Is the store generated for `family`?
pub fn supported(family: &str) -> bool {
    SUPPORTED.contains(&family)
}

/// Why the card is greyed on `family`, or `None` where it is live.
pub fn unsupported_reason(family: &str) -> Option<&'static str> {
    if supported(family) {
        return None;
    }
    Some(
        if crate::panels::mcu_module::codegen::family::is_esp(family) {
            concat!(
                "Generated for the ESP32-C3 only so far. This chip has the flash and an ",
                "esp-storage driver, but it was not compiled against it yet."
            )
        } else {
            concat!(
                "Generated for the ESP32-C3 only so far (esp-storage plus an ESP-IDF ",
                "partition table). Other families reserve flash in memory.x instead, ",
                "which is not written yet."
            )
        },
    )
}

/// The flash size a part most likely has when nothing better is known: the
/// IDE cannot read it from the chip definition, because one die ships with 2,
/// 4, 8 or 16 MB. espflash's own default makes the same guess - 2 MB for the
/// ESP32-C2, 4 MB for the rest.
pub fn default_flash_size(family: &str) -> u32 {
    if family == "esp32c2" {
        0x20_0000
    } else {
        0x40_0000
    }
}

/// Where the store lives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FlashStoreMode {
    /// A partition of its own, in a generated `partitions.csv` that flashing
    /// passes to espflash. The default: the only reservation that cannot be
    /// overwritten by the app growing into it.
    #[default]
    Partition,
    /// The default table's `nvs` partition, with no `partitions.csv` at all.
    /// Reserved by every espflash table, but in sequential-storage's format,
    /// not ESP-IDF's NVS - firmware or tools expecting real NVS there will not
    /// read it.
    Nvs,
}

impl FlashStoreMode {
    /// The token written to / read from `mcu.config`.
    pub fn token(self) -> &'static str {
        match self {
            Self::Partition => "partition",
            Self::Nvs => "nvs",
        }
    }

    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "partition" => Some(Self::Partition),
            "nvs" => Some(Self::Nvs),
            _ => None,
        }
    }

    /// What the card's selector shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Partition => "Own partition (partitions.csv)",
            Self::Nvs => "Default nvs partition (no table of its own)",
        }
    }
}

/// The store's settings. `flash_size`, `size` and `offset` are bytes; `size`
/// and `offset` only matter in [`FlashStoreMode::Partition`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FlashStoreConfig {
    pub mode: FlashStoreMode,
    pub flash_size: u32,
    pub size: u32,
    pub offset: u32,
}

impl FlashStoreConfig {
    /// Four sectors (16 KiB) at the very top of the part's likely flash size.
    pub fn default_for(family: &str) -> Self {
        let flash_size = default_flash_size(family);
        let size = 4 * SECTOR;
        Self {
            mode: FlashStoreMode::Partition,
            flash_size,
            size,
            offset: flash_size - size,
        }
    }

    /// The bytes the store owns.
    pub fn range(&self) -> Range<u32> {
        match self.mode {
            FlashStoreMode::Partition => self.offset..self.offset.saturating_add(self.size),
            FlashStoreMode::Nvs => NVS_RANGE,
        }
    }

    /// The `data` subtype the store's partition carries in the table.
    pub fn subtype(&self) -> u8 {
        match self.mode {
            FlashStoreMode::Partition => SUBTYPE_UNDEFINED,
            FlashStoreMode::Nvs => SUBTYPE_NVS,
        }
    }

    /// Does this configuration need a `partitions.csv` of its own?
    pub fn needs_partition_table(&self) -> bool {
        self.mode == FlashStoreMode::Partition
    }

    /// The app partition the generated table leaves: everything from
    /// [`APP_OFFSET`] up to the store.
    pub fn app_size(&self) -> u32 {
        self.offset.saturating_sub(APP_OFFSET)
    }

    /// What is wrong with the settings themselves, one sentence each. Empty
    /// means the generated table and the generated range are consistent.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.mode == FlashStoreMode::Nvs {
            return out;
        }
        if !self.offset.is_multiple_of(SECTOR) || !self.size.is_multiple_of(SECTOR) {
            out.push(format!(
                "The store must start and end on a 4 KiB sector: offset 0x{:X}, size 0x{:X}.",
                self.offset, self.size
            ));
        }
        if self.size < MIN_SECTORS * SECTOR {
            out.push(format!(
                "The store needs at least {MIN_SECTORS} sectors (8 KiB): sequential-storage keeps one page free for migration."
            ));
        }
        let end = u64::from(self.offset) + u64::from(self.size);
        if end > u64::from(self.flash_size) {
            out.push(format!(
                "The store ends at 0x{end:X}, past the end of a {} MB flash (0x{:X}).",
                self.flash_size / 0x10_0000,
                self.flash_size
            ));
        }
        if self.offset < APP_OFFSET + MIN_APP {
            out.push(format!(
                "The store starts at 0x{:X}, which leaves the app less than 64 KiB (it begins at 0x{APP_OFFSET:X}).",
                self.offset
            ));
        }
        out
    }

    /// The table rows the store needs: espflash's default three, with
    /// `factory` shrunk to end where the store begins, then the store.
    ///
    /// Columns are padded for the eye only - espflash trims every field. No
    /// comment may trail a row: espflash's CSV reader takes it as a field.
    pub fn partition_rows(&self) -> String {
        let rows = [
            (
                "nvs",
                "data",
                "nvs",
                NVS_RANGE.start,
                NVS_RANGE.end - NVS_RANGE.start,
            ),
            ("phy_init", "data", "phy", 0xF000, 0x1000),
            ("factory", "app", "factory", APP_OFFSET, self.app_size()),
            (LABEL, "data", "undefined", self.offset, self.size),
        ];
        let mut out = String::from("# Name,   Type, SubType, Offset, Size\n");
        for (name, ty, sub, offset, size) in rows {
            out.push_str(&format!(
                "{:<12} {:<5} {:<10} {:<9} 0x{size:X}\n",
                format!("{name},"),
                format!("{ty},"),
                format!("{sub},"),
                format!("0x{offset:X},"),
            ));
        }
        out
    }
}

/// The rows the project's `partitions.csv` must carry, or `None` when the
/// project needs no table of its own (store off, on the default `nvs`
/// partition, or not generated for `family`). THE decision, shared by the app
/// and the ESP compile harness, so the two cannot write different tables.
pub fn table_rows(store: Option<&FlashStoreConfig>, family: &str) -> Option<String> {
    store
        .filter(|c| c.needs_partition_table() && supported(family))
        .map(FlashStoreConfig::partition_rows)
}

/// One row of a partitions.csv, as far as the checks below need it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    line: usize,
    name: String,
    ty: String,
    sub: String,
    offset: Option<u32>,
    size: Option<u32>,
}

/// A number as ESP-IDF tables spell them: `0x...`, decimal, or with a `K` /
/// `M` suffix (`1M`, `24K`).
fn parse_num(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u32::from_str_radix(hex, 16).ok();
    }
    let (digits, mult) = match s.as_bytes()[s.len() - 1] {
        b'K' | b'k' => (&s[..s.len() - 1], 1024),
        b'M' | b'm' => (&s[..s.len() - 1], 1024 * 1024),
        _ => (s, 1),
    };
    digits.trim().parse::<u32>().ok()?.checked_mul(mult)
}

fn rows(csv: &str) -> Vec<Row> {
    csv.lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('#')
        })
        .map(|(i, l)| {
            let f: Vec<&str> = l.split(',').map(str::trim).collect();
            let at = |n: usize| f.get(n).copied().unwrap_or("");
            Row {
                line: i + 1,
                name: at(0).to_owned(),
                ty: at(1).to_ascii_lowercase(),
                sub: at(2).to_ascii_lowercase(),
                offset: parse_num(at(3)),
                size: parse_num(at(4)),
            }
        })
        .collect()
}

/// Is `ty` the `data` type, by name or number?
fn is_data(ty: &str) -> bool {
    ty == "data" || parse_num(ty) == Some(u32::from(TYPE_DATA))
}

/// Is `ty` the `app` type, by name or number? The only type that needs the
/// 64 KiB alignment; `data` and custom types (0x40..0xFE) need a sector.
fn is_app(ty: &str) -> bool {
    ty == "app" || parse_num(ty) == Some(0)
}

/// The `app` subtypes espflash 4.x can parse: `factory`, `ota_0`..`ota_15`,
/// `test`, or their numbers. Anything else - `0x30`, or a data subtype such
/// as `nvs` - makes it PANIC.
fn espflash_knows_app_subtype(sub: &str) -> bool {
    if sub == "factory" || sub == "test" {
        return true;
    }
    if let Some(n) = sub.strip_prefix("ota_") {
        return n.parse::<u8>().is_ok_and(|n| n <= 15);
    }
    matches!(parse_num(sub), Some(0x00 | 0x10..=0x20))
}

/// An app partition espflash writes the firmware into: `factory` or `ota_0`.
fn is_boot_app(r: &Row) -> bool {
    is_app(&r.ty)
        && (r.sub == "factory"
            || r.sub == "ota_0"
            || matches!(parse_num(&r.sub), Some(0x00 | 0x10)))
}

/// The `data` subtypes espflash 4.x can parse - names, and the numbers it has
/// names for. Anything else makes it PANIC, not refuse.
fn espflash_knows_data_subtype(sub: &str) -> bool {
    const NAMES: [&str; 11] = [
        "ota",
        "phy",
        "nvs",
        "coredump",
        "nvs_keys",
        "efuse",
        "undefined",
        "esphttpd",
        "fat",
        "spiffs",
        "littlefs",
    ];
    if NAMES.contains(&sub) {
        return true;
    }
    matches!(parse_num(sub), Some(0..=6 | 0x80..=0x83))
}

/// Does `sub` name `subtype`?
fn names_subtype(sub: &str, subtype: u8) -> bool {
    let named = match subtype {
        SUBTYPE_NVS => "nvs",
        SUBTYPE_UNDEFINED => "undefined",
        _ => "",
    };
    sub == named || parse_num(sub) == Some(u32::from(subtype))
}

/// What is wrong with `csv` as a partition table, one sentence each - and,
/// with a `store`, whether it reserves exactly the store's range: the static
/// half of "is the store really reserved?". Empty is a pass.
///
/// It checks what espflash does NOT: every row inside the flash, and the store
/// row present with exactly the range the firmware uses. And it checks first
/// what espflash would CRASH on, so the user reads a sentence instead of a
/// Rust panic in the flash log. Without a store it still runs those - a
/// hand-written table is flashed too.
///
/// `flash_size` is checked against only when the user chose it (the store's
/// Partition mode): a guess would refuse a valid table for a 16 MB module.
pub fn csv_problems(
    csv: &str,
    flash_size: Option<u32>,
    store: Option<&FlashStoreConfig>,
) -> Vec<String> {
    let mut out = Vec::new();
    let all = rows(csv);
    for r in &all {
        if is_data(&r.ty) && !espflash_knows_data_subtype(&r.sub) {
            out.push(format!(
                "Line {}: espflash cannot read the data subtype '{}' (it stops with a panic); use 'undefined'.",
                r.line, r.sub
            ));
        }
        if is_app(&r.ty) && !espflash_knows_app_subtype(&r.sub) {
            out.push(format!(
                "Line {}: espflash cannot read the app subtype '{}' (it stops with a panic); use factory, ota_0..ota_15 or test.",
                r.line, r.sub
            ));
        }
        if r.name.len() > 16 {
            out.push(format!(
                "Line {}: the name '{}' is longer than 16 characters, and espflash cuts it short.",
                r.line, r.name
            ));
        }
        let (Some(offset), Some(size)) = (r.offset, r.size) else {
            continue;
        };
        let align = if is_app(&r.ty) { APP_OFFSET } else { SECTOR };
        if !offset.is_multiple_of(align) {
            out.push(format!(
                "Line {}: '{}' starts at 0x{offset:X}, not on a 0x{align:X} boundary.",
                r.line, r.name
            ));
        }
        let end = u64::from(offset) + u64::from(size);
        if let Some(flash_size) = flash_size
            && end > u64::from(flash_size)
        {
            out.push(format!(
                "Line {}: '{}' ends at 0x{end:X}, past the end of a {} MB flash.",
                r.line,
                r.name,
                flash_size / 0x10_0000
            ));
        }
    }
    if !all.is_empty() && !all.iter().any(is_boot_app) {
        out.push(
            "No 'app, factory' (or 'ota_0') row: espflash has nowhere to write the firmware."
                .to_owned(),
        );
    }
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            if let (Some(ao), Some(asz), Some(bo), Some(bsz)) = (a.offset, a.size, b.offset, b.size)
                && ao < bo.saturating_add(bsz)
                && bo < ao.saturating_add(asz)
            {
                out.push(format!(
                    "Lines {} and {}: '{}' and '{}' overlap.",
                    a.line, b.line, a.name, b.name
                ));
            }
        }
    }
    if let Some(cfg) = store
        && store_row_missing(csv, cfg)
    {
        let want = cfg.range();
        out.push(format!(
            "No 'data, {}' row covers the store's 0x{:X}..0x{:X}: the firmware would write outside any partition.",
            subtype_name(cfg.subtype()),
            want.start,
            want.end
        ));
    }
    out
}

fn subtype_name(subtype: u8) -> &'static str {
    if subtype == SUBTYPE_NVS {
        "nvs"
    } else {
        "undefined"
    }
}

/// `csv` with every `data` subtype espflash would panic on (`0x99`, `0x40`...)
/// rewritten to `undefined` - the one change that makes such a table
/// flashable while keeping every name, offset and size. `None` when there is
/// nothing to change. The second half says what changed, one item per row.
///
/// Only the subtype field moves; the spaces after it shrink to keep the
/// columns where they were, and line endings stay as they are. A firmware
/// that looks its partition up BY that subtype must follow (to 0x06); one
/// that uses a fixed range, as the example project's does, is unaffected.
pub fn repair_data_subtypes(csv: &str) -> Option<(String, Vec<String>)> {
    const NEW: &str = "undefined";
    let mut notes = Vec::new();
    let lines: Vec<String> = csv
        .split('\n')
        .enumerate()
        .map(|(i, line)| {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                return line.to_owned();
            }
            let mut f: Vec<String> = line.split(',').map(str::to_owned).collect();
            if f.len() < 3
                || !is_data(&f[1].trim().to_ascii_lowercase())
                || espflash_knows_data_subtype(&f[2].trim().to_ascii_lowercase())
            {
                return line.to_owned();
            }
            let old = f[2].trim().to_owned();
            let lead = f[2].len() - f[2].trim_start().len();
            let trail = &f[2][lead + old.len()..];
            f[2] = format!("{}{NEW}{trail}", &f[2][..lead]);
            if let Some(next) = f.get_mut(3) {
                let spaces = next.len() - next.trim_start_matches(' ').len();
                let cut = (NEW.len().saturating_sub(old.len())).min(spaces.saturating_sub(1));
                next.drain(..cut);
            }
            notes.push(format!("line {}: '{old}' -> '{NEW}'", i + 1));
            f.join(",")
        })
        .collect();
    (!notes.is_empty()).then(|| (lines.join("\n"), notes))
}

/// Does `csv` lack the row reserving exactly `cfg`'s range? By type, subtype
/// and range - not by name, as `verify` on the chip.
pub fn store_row_missing(csv: &str, cfg: &FlashStoreConfig) -> bool {
    let want = cfg.range();
    !rows(csv).iter().any(|r| {
        is_data(&r.ty)
            && names_subtype(&r.sub, cfg.subtype())
            && r.offset == Some(want.start)
            && r.size == Some(want.end - want.start)
    })
}

/// The row a hand-written table needs for `cfg`: the default table's own
/// `nvs` row, or the store's `data, undefined` one.
pub fn store_row(cfg: &FlashStoreConfig) -> String {
    let r = cfg.range();
    let name = match cfg.mode {
        FlashStoreMode::Partition => LABEL,
        FlashStoreMode::Nvs => "nvs",
    };
    format!(
        "{name}, data, {}, 0x{:X}, 0x{:X}",
        subtype_name(cfg.subtype()),
        r.start,
        r.end - r.start
    )
}

/// Why the project's table must not be flashed, or `None` - THE gate, for
/// espflash, RTT Run and Debug alike, and the card's list.
///
/// ESP only: elsewhere a `partitions.csv` is a stray file nothing flashes. A
/// store's own settings come first (a range under two sectors would panic at
/// boot); then the table, while there is one - with none, espflash writes its
/// default, which a store in its own partition cannot live with.
pub fn flash_block(csv: &str, store: Option<&FlashStoreConfig>, family: &str) -> Option<String> {
    if !crate::panels::mcu_module::codegen::family::is_esp(family) {
        return None;
    }
    let store = store.filter(|_| supported(family));
    if let Some(c) = store {
        let p = c.problems();
        if !p.is_empty() {
            return Some(format!("Flash store: {}", p.join(" ")));
        }
    }
    if csv.trim().is_empty() {
        return store.filter(|c| c.needs_partition_table()).map(|_| {
            "partitions.csv is empty, but the flash store needs its own partition in it.".to_owned()
        });
    }
    let known_size = store
        .filter(|c| c.needs_partition_table())
        .map(|c| c.flash_size);
    let problems = csv_problems(csv, known_size, store);
    (!problems.is_empty()).then(|| format!("{TABLE_BLOCK_PREFIX} {}", problems.join(" ")))
}

/// How [`flash_block`] starts a block that is the TABLE's - the only kind
/// the Flash tab's "Fix partitions.csv" may be offered for.
pub const TABLE_BLOCK_PREFIX: &str = "partitions.csv:";

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> FlashStoreConfig {
        FlashStoreConfig::default_for("esp32c3")
    }

    /// The default is four sectors at the top of a 4 MB part, and the table it
    /// writes passes its own check - the two halves can never disagree.
    #[test]
    fn the_default_store_is_the_top_16_kib_and_its_table_passes() {
        let c = cfg();
        assert_eq!(c.range(), 0x3F_C000..0x40_0000);
        assert!(c.problems().is_empty(), "{:?}", c.problems());
        let rows = c.partition_rows();
        assert!(rows.contains("factory,"), "{rows}");
        assert!(
            rows.contains("0x3EC000"),
            "the app ends where the store starts: {rows}"
        );
        assert!(
            csv_problems(&rows, Some(c.flash_size), Some(&c)).is_empty(),
            "{:?}\n{rows}",
            csv_problems(&rows, Some(c.flash_size), Some(&c))
        );
        // espflash's 2 MB guess on the C2.
        assert_eq!(
            FlashStoreConfig::default_for("esp32c2").range().end,
            0x20_0000
        );
    }

    /// The example project's row: espflash panics on it, so the check says so
    /// in a sentence - and it does not cover the store either.
    #[test]
    fn a_numeric_custom_subtype_is_reported_before_espflash_panics_on_it() {
        let c = FlashStoreConfig {
            offset: 0x3F_0000,
            size: 0x4000,
            ..cfg()
        };
        let example = "# Name, Type, SubType, Offset, Size\n\
                       nvs, data, nvs, 0x9000, 0x6000\n\
                       phy_init, data, phy, 0xf000, 0x1000\n\
                       factory, app, factory, 0x10000, 0x3E0000\n\
                       cfg, data, 0x99, 0x3F0000, 0x4000\n";
        let p = csv_problems(example, Some(c.flash_size), Some(&c));
        assert!(
            p.iter()
                .any(|s| s.contains("'0x99'") && s.contains("panic")),
            "{p:?}"
        );
        assert!(
            p.iter().any(|s| s.contains("No 'data, undefined'")),
            "{p:?}"
        );
        // The same row with `undefined` passes.
        let fixed = example.replace("0x99", "undefined");
        assert!(
            csv_problems(&fixed, Some(c.flash_size), Some(&c)).is_empty(),
            "{:?}",
            csv_problems(&fixed, Some(c.flash_size), Some(&c))
        );
    }

    /// espflash sums sizes and never checks `offset + size`; this does.
    #[test]
    fn a_row_past_the_end_of_flash_and_an_overlap_are_caught() {
        let c = cfg();
        let past = c.partition_rows().replace("0x3FC000", "0x400000");
        assert!(
            csv_problems(&past, Some(c.flash_size), Some(&c))
                .iter()
                .any(|s| s.contains("past the end")),
            "{:?}",
            csv_problems(&past, Some(c.flash_size), Some(&c))
        );
        let overlap = c.partition_rows().replace("0x3EC000", "0x3F0000");
        assert!(
            csv_problems(&overlap, Some(c.flash_size), Some(&c))
                .iter()
                .any(|s| s.contains("overlap")),
            "{:?}",
            csv_problems(&overlap, Some(c.flash_size), Some(&c))
        );
    }

    /// What espflash 4.4.0 accepts is not refused: a 16 MB table when nothing
    /// says the flash is smaller, a custom type on a 4 KiB boundary. What it
    /// panics on, or cannot flash into, is.
    #[test]
    fn a_valid_hand_written_table_passes_and_an_unflashable_one_does_not() {
        let big = "nvs, data, nvs, 0x9000, 0x6000\n\
                   factory, app, factory, 0x10000, 0xF00000\n\
                   mine, 0x40, 0x01, 0xF18000, 0x8000\n";
        assert!(
            csv_problems(big, None, None).is_empty(),
            "{:?}",
            csv_problems(big, None, None)
        );
        assert!(
            csv_problems(big, Some(0x40_0000), None)
                .iter()
                .any(|s| s.contains("past the end")),
            "a size the user chose is still checked"
        );
        for bad in ["0x30", "nvs"] {
            let t = format!(
                "factory, app, factory, 0x10000, 0x100000\nx, app, {bad}, 0x110000, 0x10000\n"
            );
            assert!(
                csv_problems(&t, None, None)
                    .iter()
                    .any(|s| s.contains(&format!("app subtype '{bad}'"))),
                "{bad}"
            );
        }
        let no_app = "nvs, data, nvs, 0x9000, 0x6000\nmine, data, fat, 0x3F0000, 0x8000\n";
        assert!(
            csv_problems(no_app, None, None)
                .iter()
                .any(|s| s.contains("nowhere to write the firmware"))
        );
        let ota = "otadata, data, ota, 0xd000, 0x2000\nota_0, app, ota_0, 0x10000, 0x100000\n";
        assert!(csv_problems(ota, None, None).is_empty());
    }

    /// The example project's own table, CRLF and no final newline as it is
    /// on disk: the repair touches the subtype only, the result passes, and a
    /// table with nothing espflash panics on is left alone.
    #[test]
    fn a_panicking_data_subtype_is_repaired_to_undefined_and_nothing_else_moves() {
        let theirs = concat!(
            "# Name,   Type, SubType, Offset,   Size\r\n",
            "nvs,      data, nvs,     0x9000,   0x6000\r\n",
            "phy_init, data, phy,     0xf000,   0x1000\r\n",
            "factory,  app,  factory, 0x10000,  0x3E0000\r\n",
            "cfg,      data, 0x99,    0x3F0000, 0x4000",
        );
        assert!(!csv_problems(theirs, None, None).is_empty());
        let (fixed, notes) = repair_data_subtypes(theirs).expect("something to repair");
        assert_eq!(notes, ["line 5: '0x99' -> 'undefined'"]);
        assert!(
            fixed.ends_with("cfg,      data, undefined, 0x3F0000, 0x4000"),
            "{fixed:?}"
        );
        // Every other line, its line ending included, is byte-for-byte the same.
        let (a, b) = (
            theirs.rsplit_once("\r\n").unwrap(),
            fixed.rsplit_once("\r\n").unwrap(),
        );
        assert_eq!(a.0, b.0);
        assert!(
            csv_problems(&fixed, None, None).is_empty(),
            "{:?}",
            csv_problems(&fixed, None, None)
        );
        assert_eq!(repair_data_subtypes(&fixed), None, "nothing left to repair");
        // No spaces to give back: nothing is cut, the field just grows.
        let tight = "x, data, 0x40,0x3F0000, 0x4000\n";
        let (t, _) = repair_data_subtypes(tight).unwrap();
        assert_eq!(t, "x, data, undefined,0x3F0000, 0x4000\n");
        // An app row's bad subtype is not guessed at.
        assert_eq!(
            repair_data_subtypes("a, app, 0x30, 0x10000, 0x10000\n"),
            None
        );
    }

    /// The one gate every flashing path asks.
    #[test]
    fn the_flash_gate_asks_the_settings_then_the_table_and_only_on_esp() {
        let c = cfg();
        let table = c.partition_rows();
        assert_eq!(flash_block(&table, Some(&c), "esp32c3"), None);
        // A stray file elsewhere is nobody's business.
        assert_eq!(
            flash_block("garbage, data, 0x99, 1, 1\n", None, "stm32f1"),
            None
        );
        // One sector passes every table check, and panics at boot.
        let one = FlashStoreConfig {
            size: SECTOR,
            offset: 0x3F_F000,
            ..c
        };
        let rows = one.partition_rows();
        assert!(csv_problems(&rows, Some(one.flash_size), Some(&one)).is_empty());
        assert!(flash_block(&rows, Some(&one), "esp32c3").is_some_and(|w| w.contains("2 sectors")));
        // Its own partition with no table at all: espflash's default would
        // put the store inside the app.
        assert!(flash_block("", Some(&c), "esp32c3").is_some());
        let nvs = FlashStoreConfig {
            mode: FlashStoreMode::Nvs,
            ..c
        };
        assert_eq!(flash_block("", Some(&nvs), "esp32c3"), None);
        assert_eq!(store_row(&nvs), "nvs, data, nvs, 0x9000, 0x6000");
        assert_eq!(
            store_row(&c),
            "flash_store, data, undefined, 0x3FC000, 0x4000"
        );
        assert!(!store_row_missing(&table, &c));
        assert!(
            !store_row_missing(&table, &nvs),
            "the generated table keeps the nvs row"
        );
        assert!(store_row_missing(
            "factory, app, factory, 0x10000, 0x100000\n",
            &nvs
        ));
    }

    #[test]
    fn the_settings_are_checked_on_their_own() {
        // Misaligned, one sector, and running 0x800 past the end of 4 MB.
        let bad = FlashStoreConfig {
            offset: 0x3F_F800,
            size: 0x1000,
            ..cfg()
        };
        let p = bad.problems();
        assert!(p.iter().any(|s| s.contains("sector")), "{p:?}");
        assert!(p.iter().any(|s| s.contains("at least 2 sectors")), "{p:?}");
        assert!(p.iter().any(|s| s.contains("past the end")), "{p:?}");
        let squeezed = FlashStoreConfig {
            offset: 0x1_8000,
            ..cfg()
        };
        assert!(squeezed.problems().iter().any(|s| s.contains("64 KiB")));
        // The nvs mode has nothing to configure, so nothing to get wrong.
        let nvs = FlashStoreConfig {
            mode: FlashStoreMode::Nvs,
            ..bad
        };
        assert!(nvs.problems().is_empty());
        assert_eq!(nvs.range(), NVS_RANGE);
        assert_eq!(nvs.subtype(), SUBTYPE_NVS);
    }

    #[test]
    fn numbers_read_the_way_esp_idf_tables_write_them() {
        assert_eq!(parse_num("0x3F0000"), Some(0x3F_0000));
        assert_eq!(parse_num("24K"), Some(0x6000));
        assert_eq!(parse_num("1M"), Some(0x10_0000));
        assert_eq!(parse_num("4096"), Some(4096));
        assert_eq!(parse_num(""), None);
        assert_eq!(parse_num("x"), None);
    }

    #[test]
    fn only_the_c3_is_live_and_every_other_chip_says_why() {
        assert!(supported("esp32c3"));
        assert_eq!(unsupported_reason("esp32c3"), None);
        for f in [
            "esp32", "esp32s3", "esp32c6", "stm32f1", "rp2040", "nrf52833",
        ] {
            assert!(!supported(f), "{f}");
            assert!(
                unsupported_reason(f).is_some_and(|r| !r.contains("  ")),
                "{f}"
            );
        }
        assert!(LABEL.len() <= 16);
    }

    #[test]
    fn the_mode_token_round_trips() {
        for m in [FlashStoreMode::Partition, FlashStoreMode::Nvs] {
            assert_eq!(FlashStoreMode::from_token(m.token()), Some(m));
        }
        assert_eq!(FlashStoreMode::from_token("bogus"), None);
    }
}
