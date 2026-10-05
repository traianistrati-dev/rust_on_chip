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
//!
//! # STM32
//!
//! No partition table: the store is the last pages of flash, and `memory.x`'s
//! FLASH region shrinks by exactly that much, so code that grows into the
//! store fails to LINK ([`FlashStoreMode::MemoryX`]). Every flashing path the
//! IDE has (cargo flash, probe-rs run, the Debug launch, OpenOCD `program`)
//! erases only the sectors it writes, so the store survives a reflash; only a
//! chip erase wipes it. The firmware side is embassy-stm32's blocking `Flash`,
//! or on the F1's own HAL an adapter over `FlashWriter` - see
//! [`platform`] for which, and for every part it is NOT generated on and why.
//! The page sizes come from [`super::stm32_flash_geometry`], harvested from
//! the metadata embassy-stm32 itself is built from.

use std::ops::Range;

use crate::panels::mcu_module::mcu::{Mcu, Runtime};
use crate::panels::mcu_module::stm32_flash_geometry as geo;

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

/// The ESP families the store is generated for. ESP32-C3 first: it is the chip
/// the feature was compiled and run against. The others have the FLASH
/// peripheral and an esp-storage feature, but two need more than this
/// generator writes - the ESP32 and S3 must park their second core before a
/// write, and the ESP32 refuses the Debug build's `opt-level = 1` in
/// esp-storage's build script.
pub const SUPPORTED: &[&str] = &["esp32c3"];

/// Is the ESP version of the store (esp-storage, partitions.csv) generated
/// for `family`? STM32 goes through [`platform`].
pub fn supported(family: &str) -> bool {
    SUPPORTED.contains(&family)
}

/// The largest STM32 erase unit the store is laid out on: 16 KiB. Past it are
/// only the 32/64/128/256 KiB sectors of F2/F4/F7 and the H7, where the two
/// pages the store needs would be 256 KiB at least.
pub const MAX_PAGE: u32 = 16 * 1024;

/// The largest sector the [`Layout::AfterVectors`] store is laid out on: the
/// F2/F4/F72x start with four of 16 KiB, the F74x/F75x with four of 32 KiB.
pub const MAX_HEAD_PAGE: u32 = 32 * 1024;

/// What generates the store on a chip, under a runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// esp-storage, reserved in an ESP-IDF partition table.
    Esp,
    /// The chip's own flash through its HAL, reserved in `memory.x`.
    Stm32 {
        hal: StmHal,
        geo: Geometry,
        layout: Layout,
    },
}

/// Where an STM32 store sits, and so what memory.x does about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Layout {
    /// The last pages of flash: memory.x's FLASH ends where they begin.
    End,
    /// F2/F4/F7, whose flash ENDS in 128/256 KiB sectors but starts with four
    /// small ones: the store takes the sectors right after the vector table's
    /// (sector 0), and memory.x starts the program after them (`_stext`) - the
    /// layout of ST's AN3969. Sector 0 keeps the vector table alone; the rest of
    /// it is the price.
    AfterVectors,
}

/// Which STM32 HAL writes the flash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StmHal {
    /// embassy-stm32's blocking `Flash` - every family, the F1 on Async.
    Embassy,
    /// stm32f1xx-hal's `FlashWriter`, behind a generated `NorFlash` adapter -
    /// the F1 on Blocking and Native.
    F1Hal,
}

/// A part's flash, as far as the store needs it. Bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Geometry {
    /// The whole flash (embassy-stm32's `FLASH_SIZE`).
    pub size: u32,
    /// The erase unit at the end of flash, where the store lives.
    pub page: u32,
    /// The write unit - sequential-storage's word.
    pub write: u32,
    /// The erase unit at the START of flash, and the size of the region made
    /// of it (embassy-stm32's `BANK1_REGION1`): 16 KiB x 4 on an F4.
    pub head_page: u32,
    pub head_size: u32,
}

impl Geometry {
    /// The erase unit the store is laid out in on `layout`.
    pub fn store_page(&self, layout: Layout) -> u32 {
        match layout {
            Layout::End => self.page,
            Layout::AfterVectors => self.head_page,
        }
    }
}

/// The flash geometry of an STM32 part, with the table's flags, by name -
/// `STM32G431CBUx`, `stm32f103c8t6` and `stm32g431cb` all find `stm32g431cb`
/// (the longest table name the part's name starts with). `None` for a part
/// stm32-metapac does not know, and for the N6, which has no internal flash.
pub fn geometry(part: &str) -> Option<(Geometry, u8)> {
    let slug: String = part
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    (9..=slug.len()).rev().find_map(|n| {
        let i = geo::PARTS
            .binary_search_by(|(name, ..)| (*name).cmp(&slug[..n]))
            .ok()?;
        let (_, kib, shape) = geo::PARTS[i];
        let s = geo::SHAPES[usize::from(shape)];
        Some((
            Geometry {
                size: u32::from(kib) * 1024,
                page: s.page,
                write: u32::from(s.write),
                head_page: s.head_page,
                head_size: s.head_size,
            },
            s.flags,
        ))
    })
}

/// The name the table knows `mcu`'s part by: its display name, or else its
/// definition id - a chip the user renamed in the MCU form ("Blue Pill") still
/// has `stm32f103c8t6` as its id.
pub fn part_of(mcu: &Mcu) -> &str {
    [mcu.name.as_str(), mcu.id.as_str()]
        .into_iter()
        .find(|n| geometry(n).is_some())
        .unwrap_or(&mcu.name)
}

/// [`platform`] for `mcu`'s chip and runtime - what every caller asks.
pub fn platform_of(mcu: &Mcu) -> Result<Platform, String> {
    platform(&mcu.family, part_of(mcu), mcu.runtime)
}

/// How the store is generated for `part` of `family` under `runtime`, or the
/// sentence the card shows where it is not. THE decision: the card, the
/// generators, `memory.x` and the flash check all ask it.
///
/// Not on STM32 parts whose last two pages cost too much (F2/F4/F7 sectors of
/// 128/256 KiB, the H7's 128 KiB: [`MAX_PAGE`]; or two pages over a quarter
/// of a small flash), nor where the plain end-of-flash layout is wrong: banks
/// chosen in the option bytes (the page size follows them, and embassy's
/// driver panics - or on the L552xC erases the wrong page - when the
/// project's bank setup disagrees), banks with a gap between them, the F1's
/// XL parts (bank 2 is not driven), flash that erases to 0x00 (L0/L1), the
/// WB's radio stack, and the lines embassy-stm32 0.6 cannot write (H7RS, U3,
/// WB0). RTIC is left for later: the store would be one of its `Local`
/// resources.
pub fn platform(family: &str, part: &str, runtime: Runtime) -> Result<Platform, String> {
    use crate::panels::mcu_module::codegen::family as fam;
    if supported(family) {
        return Ok(Platform::Esp);
    }
    if fam::is_esp(family) {
        return Err(concat!(
            "Generated for the ESP32-C3 only so far. This chip has the flash and an ",
            "esp-storage driver, but it was not compiled against it yet."
        )
        .to_owned());
    }
    if !family.starts_with("stm32") {
        return Err(concat!(
            "Generated for the ESP32-C3 and STM32 so far - this family's flash ",
            "driver is not wired in yet."
        )
        .to_owned());
    }
    if runtime == Runtime::Rtic && fam::rtic_supported(family) {
        return Err(concat!(
            "Not on the RTIC runtime yet: the store would be one of the app's ",
            "Local resources, which the generator does not write. Blocking, Native ",
            "and Async have it."
        )
        .to_owned());
    }
    let Some((g, flags)) = geometry(part) else {
        return Err(if family == "stm32n6" {
            "The STM32N6 has no internal flash to keep settings in.".to_owned()
        } else {
            format!(
                "{part} is not in stm32-metapac 21's part list, so its flash page size is unknown."
            )
        });
    };
    let slug = part.to_ascii_lowercase();
    let line = |p: &str| slug.starts_with(p);
    if line("stm32h7r") || line("stm32h7s") || line("stm32u3") || line("stm32wb0") {
        return Err(concat!(
            "embassy-stm32 0.6 cannot write or erase this line's flash yet (its ",
            "driver is a stub there)."
        )
        .to_owned());
    }
    // embassy-stm32 0.6's L4/WL driver (l.rs) erases and writes with the
    // flash's data cache left on and never resets it - g.rs, f2.rs and f4.rs
    // do (DCEN off, DCRST, DCEN on). sequential-storage reads a page, erases
    // or writes it, and reads it again, so it could read the cached old bytes.
    if line("stm32l4") || line("stm32wl") {
        return Err(concat!(
            "embassy-stm32 0.6 does not reset this line's flash data cache after an ",
            "erase or a write, so the store could read stale bytes back. Not ",
            "generated for it yet."
        )
        .to_owned());
    }
    if line("stm32wb") && !line("stm32wba") {
        return Err(concat!(
            "The top of the WB's flash holds the radio coprocessor's stack, and ",
            "writing next to it needs the CPU2 handshake - not generated yet."
        )
        .to_owned());
    }
    if flags & geo::ERASED_FF == 0 {
        return Err(concat!(
            "This flash reads 0x00 when erased, and sequential-storage needs 0xFF. ",
            "The chip's data EEPROM would be the place - not generated yet."
        )
        .to_owned());
    }
    // The L552xC is the configurable case metapac does not mark: one 4 KiB-page
    // bank in its data, but DBANK (with DB256K) set from the factory - two banks
    // of 2 KiB pages. embassy's L5 erase counts 4 KiB pages and, with DBANK
    // set, never picks bank 2 below 256 pages, so the store's last pages
    // would erase PROGRAM flash in bank 1 (embassy-stm32 0.6 l.rs). The rest
    // of the L5 line is marked; the family goes as one.
    if flags & geo::CONFIGURABLE != 0 || line("stm32l5") {
        return Err(concat!(
            "Single or dual bank is chosen in this part's option bytes, and the page ",
            "size follows the choice; embassy's flash driver panics, or erases the ",
            "wrong page, when it disagrees with the project's bank setup. Not ",
            "generated for it yet."
        )
        .to_owned());
    }
    if flags & geo::CONTIGUOUS == 0 {
        return Err(concat!(
            "This part's two flash banks have a gap between them, so the end of ",
            "flash is not where FLASH_SIZE says. Not generated for it yet."
        )
        .to_owned());
    }
    if family == "stm32f1" && g.size > 512 * 1024 {
        return Err(concat!(
            "On an XL-density F1 the end of flash is bank 2, which the F1 drivers ",
            "do not program. Not generated for it yet."
        )
        .to_owned());
    }
    let hal = if fam::uses_stm32f1xx_hal(family, runtime) {
        StmHal::F1Hal
    } else {
        StmHal::Embassy
    };
    // Equal pages of up to 16 KiB: the last two, at the end of flash.
    if flags & geo::UNIFORM != 0 && g.page <= MAX_PAGE {
        if u64::from(g.page) * 2 * 4 > u64::from(g.size) {
            return Err(format!(
                concat!(
                    "Two pages of {} KiB - the store's minimum - would be {}% of this ",
                    "{} KiB flash. Not generated for it."
                ),
                g.page / 1024,
                u64::from(g.page) * 200 / u64::from(g.size),
                g.size / 1024,
            ));
        }
        return Ok(Platform::Stm32 {
            hal,
            geo: g,
            layout: Layout::End,
        });
    }
    // F2/F4/F7: small sectors first, big ones at the end - the store goes right
    // after the vector table's sector, through embassy's first flash region.
    let head_sectors = g.head_size / g.head_page.max(1);
    if flags & geo::UNIFORM == 0
        && g.head_page <= MAX_HEAD_PAGE
        && head_sectors >= MIN_SECTORS + 1
        && hal == StmHal::Embassy
    {
        // Sector 0 (the vector table, the rest of it unused) plus two sectors.
        let cost = u64::from(g.head_page) * u64::from(MIN_SECTORS + 1);
        if cost * 2 > u64::from(g.size) {
            return Err(format!(
                concat!(
                    "The vector table's sector and the store's two after it would be {} ",
                    "KiB of this {} KiB flash. Not generated for it."
                ),
                cost / 1024,
                g.size / 1024,
            ));
        }
        return Ok(Platform::Stm32 {
            hal,
            geo: g,
            layout: Layout::AfterVectors,
        });
    }
    // Equal sectors of 32 KiB and up (the H7's 128 KiB): two of them are 256
    // KiB or more, and there are no small ones to use instead.
    Err(format!(
        concat!(
            "This flash erases in sectors of {} KiB, and the store needs two of them - ",
            "{} KiB. Not generated for it."
        ),
        g.page / 1024,
        g.page * 2 / 1024,
    ))
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
    /// STM32: the last pages of flash, kept out of `memory.x`'s FLASH region.
    /// `offset` is from the start of flash, as embassy-stm32's `Flash` counts.
    MemoryX,
}

impl FlashStoreMode {
    /// The token written to / read from `mcu.config`.
    pub fn token(self) -> &'static str {
        match self {
            Self::Partition => "partition",
            Self::Nvs => "nvs",
            Self::MemoryX => "memoryx",
        }
    }

    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "partition" => Some(Self::Partition),
            "nvs" => Some(Self::Nvs),
            "memoryx" => Some(Self::MemoryX),
            _ => None,
        }
    }

    /// What the card's selector shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Partition => "Own partition (partitions.csv)",
            Self::Nvs => "Default nvs partition (no table of its own)",
            Self::MemoryX => "End of flash, kept out of memory.x",
        }
    }
}

/// The store's settings. `flash_size`, `size` and `offset` are bytes; `size`
/// and `offset` matter in [`FlashStoreMode::Partition`] and
/// [`FlashStoreMode::MemoryX`].
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

    /// The default on an STM32: the last [`MIN_SECTORS`] pages - 2 KiB on an
    /// F103C8, 4 KiB on a G431, 16 KiB on a U5. Small on purpose: an F103C8
    /// has 64 KiB in all, and the ESP's 16 KiB would be a quarter of it.
    pub fn default_stm32(g: &Geometry) -> Self {
        let size = MIN_SECTORS * g.page;
        Self {
            mode: FlashStoreMode::MemoryX,
            flash_size: g.size,
            size,
            offset: g.size - size,
        }
    }

    /// The default on an F2/F4/F7 ([`Layout::AfterVectors`]): the two sectors
    /// after the vector table's - 32 KiB from 0x08004000 on an F4.
    pub fn default_after_vectors(g: &Geometry) -> Self {
        Self {
            mode: FlashStoreMode::MemoryX,
            flash_size: g.size,
            size: MIN_SECTORS * g.head_page,
            offset: g.head_page,
        }
    }

    /// The default for whatever `platform` the chip is on.
    pub fn default_on(platform: &Platform, family: &str) -> Self {
        match platform {
            Platform::Esp => Self::default_for(family),
            Platform::Stm32 {
                geo,
                layout: Layout::End,
                ..
            } => Self::default_stm32(geo),
            Platform::Stm32 {
                geo,
                layout: Layout::AfterVectors,
                ..
            } => Self::default_after_vectors(geo),
        }
    }

    /// The bytes the store owns.
    pub fn range(&self) -> Range<u32> {
        match self.mode {
            FlashStoreMode::Partition | FlashStoreMode::MemoryX => {
                self.offset..self.offset.saturating_add(self.size)
            }
            FlashStoreMode::Nvs => NVS_RANGE,
        }
    }

    /// The `data` subtype the store's partition carries in the table.
    pub fn subtype(&self) -> u8 {
        match self.mode {
            FlashStoreMode::Partition | FlashStoreMode::MemoryX => SUBTYPE_UNDEFINED,
            FlashStoreMode::Nvs => SUBTYPE_NVS,
        }
    }

    /// What is wrong with the settings on `platform`, one sentence each:
    /// [`Self::problems`] on an ESP, the page and end-of-flash rules on an
    /// STM32. Settings saved for the other platform (a chip changed under a
    /// project) say so, and the card's Reset fixes them.
    pub fn problems_on(&self, platform: &Platform) -> Vec<String> {
        let Platform::Stm32 {
            geo: g, layout, ..
        } = platform
        else {
            if self.mode == FlashStoreMode::MemoryX {
                return vec![
                    "These settings are for an STM32's memory.x, not this chip.".to_owned(),
                ];
            }
            return self.problems();
        };
        if self.mode != FlashStoreMode::MemoryX {
            return vec![
                "These settings are for an ESP partition table, not this chip.".to_owned(),
            ];
        }
        let mut out = Vec::new();
        if self.flash_size != g.size {
            out.push(format!(
                "Saved for a {} KiB flash; this part has {} KiB.",
                self.flash_size / 1024,
                g.size / 1024
            ));
        }
        let page = g.store_page(*layout).max(1);
        if !self.size.is_multiple_of(page) {
            out.push(format!(
                "The store must be whole pages of {} KiB: 0x{:X} is not.",
                page / 1024,
                self.size
            ));
        }
        if self.size < MIN_SECTORS * page {
            out.push(format!(
                "The store needs at least {MIN_SECTORS} pages ({} KiB): sequential-storage keeps one free for migration.",
                MIN_SECTORS * page / 1024
            ));
        }
        let end = u64::from(self.offset) + u64::from(self.size);
        match layout {
            Layout::End if end != u64::from(g.size) => out.push(format!(
                "The store must end where the flash ends (0x{:X}); it ends at 0x{end:X}.",
                g.size,
            )),
            Layout::AfterVectors if self.offset != g.head_page => out.push(format!(
                "The store must start right after the vector table's sector (0x{:X}); it starts at 0x{:X}.",
                g.head_page, self.offset,
            )),
            Layout::AfterVectors if end > u64::from(g.head_size) => out.push(format!(
                "The store must stay in the first {} KiB of {} KiB sectors; it ends at 0x{end:X}.",
                g.head_size / 1024,
                g.head_page / 1024,
            )),
            _ => {}
        }
        if self.size > g.size / 2 {
            out.push(format!(
                "The store takes {} of {} KiB - more than half the flash, which the program needs.",
                self.size / 1024,
                g.size / 1024
            ));
        }
        out
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
        if self.mode != FlashStoreMode::Partition {
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
        FlashStoreMode::Partition | FlashStoreMode::MemoryX => LABEL,
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
        let p = c.problems_on(&Platform::Esp);
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

/// Where every STM32's flash starts; the store's offsets count from here.
pub const STM32_FLASH_BASE: u32 = 0x0800_0000;

/// Why an STM32 project with a flash store must not be flashed, or `None`.
///
/// The store's own settings first. Then `memory.x`: the IDE's block always
/// keeps FLASH out of the store, but a memory.x without the markers is the
/// user's, never rewritten - and one whose FLASH runs into the store would
/// let the program grow over the settings, or the store erase the program.
/// Nothing to check while the store is off or not generated for the part.
pub fn stm32_flash_block(
    memory_x: &str,
    store: Option<&FlashStoreConfig>,
    family: &str,
    part: &str,
    runtime: Runtime,
) -> Option<String> {
    let store = store?;
    let p @ Platform::Stm32 { layout, .. } = platform(family, part, runtime).ok()? else {
        return None;
    };
    let problems = store.problems_on(&p);
    if !problems.is_empty() {
        return Some(format!("Flash store: {}", problems.join(" ")));
    }
    memory_x_overlap(memory_x, store, layout)
}

/// The sentence for a `memory.x` that would link the program over the store,
/// or `None`. Only a memory.x the user wrote can: the IDE's block always keeps
/// them apart. What cannot be read is not refused on a guess.
///
/// - [`Layout::End`]: FLASH must end where the store begins.
/// - [`Layout::AfterVectors`]: FLASH must start before the store (sector 0
///   holds the vector table), and `_stext` must start the program after it.
pub fn memory_x_overlap(memory_x: &str, store: &FlashStoreConfig, layout: Layout) -> Option<String> {
    let (origin, length) = memory_x_flash(memory_x)?;
    let start = u64::from(STM32_FLASH_BASE) + u64::from(store.offset);
    let store_end = start + u64::from(store.size);
    match layout {
        Layout::End => {
            let end = origin + length;
            (end > start).then(|| {
                format!(
                    concat!(
                        "memory.x: FLASH runs to 0x{:X}, into the flash store at 0x{:X}. Shrink ",
                        "its LENGTH to {}K, or let the IDE keep memory.x (its GENERATED markers)."
                    ),
                    end,
                    start,
                    start.saturating_sub(origin) / 1024
                )
            })
        }
        Layout::AfterVectors if origin > start => Some(format!(
            concat!(
                "memory.x: FLASH starts at 0x{:X}, past the flash store at 0x{:X} - the ",
                "store sits right after the vector table, so FLASH must start at 0x{:08X}."
            ),
            origin, start, STM32_FLASH_BASE
        )),
        Layout::AfterVectors => match memory_x_symbol(memory_x, "_stext") {
            None => Some(format!(
                concat!(
                    "memory.x: the program must start after the flash store - add ",
                    "`_stext = 0x{:08X};`, or let the IDE keep memory.x (its GENERATED ",
                    "markers)."
                ),
                store_end
            )),
            Some(Some(text)) if text < store_end => Some(format!(
                concat!(
                    "memory.x: _stext = 0x{:X} starts the program inside the flash store ",
                    "(0x{:X}..0x{:X}); set `_stext = 0x{:08X};`."
                ),
                text, start, store_end, store_end
            )),
            Some(_) => None,
        },
    }
}

/// `name = <expr>;` in a linker script: `None` when it is not assigned,
/// `Some(None)` when its value is not plain numbers (a symbol, a function).
fn memory_x_symbol(text: &str, name: &str) -> Option<Option<u64>> {
    let mut clean = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("/*") {
        clean.push_str(&rest[..at]);
        rest = rest[at..].find("*/").map_or("", |e| &rest[at + e + 2..]);
    }
    clean.push_str(rest);
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.';
    clean.match_indices(name).find_map(|(at, _)| {
        let before = clean[..at].chars().next_back();
        if before.is_some_and(ident) {
            return None;
        }
        let after = clean[at + name.len()..].trim_start();
        let rhs = after.strip_prefix('=')?;
        if rhs.starts_with('=') {
            return None;
        }
        let value = rhs.split(';').next().unwrap_or("");
        // `PROVIDE(_stext = 0x…);` keeps its closing parenthesis.
        Some(ld_expr(value.trim().trim_end_matches(')')))
    })
}

/// memory.x's `FLASH` region as (origin, length), read the way ld reads the
/// forms written by hand - numbers with a `K`/`M` suffix joined by `+` and `-`
/// (`LENGTH = 64K - 2K`, `ORIGIN = 0x08000000 + 16K`) - and only the region
/// named exactly `FLASH`, the one cortex-m-rt links the program into.
/// `None` when there is none, or it uses what this cannot evaluate (a symbol,
/// `ORIGIN(...)`): then nothing is refused on a guess.
///
/// Not `size::parse_memory_x`: that reads the first `FLASH*` region and
/// stops at the first space, which is all the Size bar needs and would read
/// `64K - 2K` as 64K here - refusing a memory.x that reserves the store
/// exactly.
pub fn memory_x_flash(text: &str) -> Option<(u64, u64)> {
    let mut clean = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("/*") {
        clean.push_str(&rest[..at]);
        rest = rest[at..].find("*/").map_or("", |e| &rest[at + e + 2..]);
    }
    clean.push_str(rest);
    clean.lines().find_map(|line| {
        let (name, spec) = line.split_once(':')?;
        // `FLASH (rx) :`, and `MEMORY { FLASH :` on one line: the last word
        // before the attributes.
        let name = name.split('(').next().unwrap_or(name);
        let name = name
            .trim()
            .rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .next()
            .unwrap_or("");
        if name != "FLASH" {
            return None;
        }
        let mut origin = None;
        let mut length = None;
        for field in spec.split(',') {
            let (key, value) = field.split_once('=')?;
            let value = ld_expr(value.trim().trim_end_matches('}'))?;
            match key.trim().to_ascii_uppercase().as_str() {
                "ORIGIN" | "ORG" | "O" => origin = Some(value),
                "LENGTH" | "LEN" | "L" => length = Some(value),
                _ => return None,
            }
        }
        Some((origin?, length?))
    })
}

/// An ld expression of numbers (`0x…`, decimal, `K`/`M` suffix) joined by
/// `+` and `-`, or `None` for anything else.
fn ld_expr(s: &str) -> Option<u64> {
    let s = s.trim();
    let mut total: i128 = 0;
    let mut sign: i128 = 1;
    let mut want_term = true;
    let mut chars = s.char_indices().peekable();
    while let Some(&(i, c)) = chars.peek() {
        match c {
            ' ' | '\t' => {
                chars.next();
            }
            '+' | '-' if !want_term => {
                sign = if c == '-' { -1 } else { 1 };
                want_term = true;
                chars.next();
            }
            _ if want_term && c.is_ascii_alphanumeric() => {
                let mut end = i;
                while let Some(&(j, d)) = chars.peek() {
                    if d.is_ascii_alphanumeric() {
                        end = j + d.len_utf8();
                        chars.next();
                    } else {
                        break;
                    }
                }
                let term = crate::size::parse_ld_number(&s[i..end])?;
                total += sign * i128::from(term);
                want_term = false;
            }
            _ => return None,
        }
    }
    if want_term {
        return None;
    }
    u64::try_from(total).ok()
}

/// The store's bytes, from the start of flash, that `memory.x` keeps out of
/// FLASH - or `None` when there is no STM32 store to reserve (off, an ESP, a
/// part or runtime it is not generated on). The same decision as the
/// generated `flash_store.rs`, so the file's `STORE_RANGE` and memory.x
/// always describe the same pages; settings with problems are still
/// reserved as they are, and the flash check refuses them.
pub fn stm32_reservation(
    store: Option<&FlashStoreConfig>,
    family: &str,
    part: &str,
    runtime: Runtime,
) -> Option<Reservation> {
    let store = store.filter(|c| c.mode == FlashStoreMode::MemoryX)?;
    match platform(family, part, runtime) {
        Ok(Platform::Stm32 { layout, .. }) => Some(Reservation {
            range: store.range(),
            layout,
        }),
        _ => None,
    }
}

/// What memory.x reserves for an STM32 store: its bytes from the start of
/// flash, and how they are kept out of the program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reservation {
    pub range: Range<u32>,
    pub layout: Layout,
}

/// [`flash_block`] or [`stm32_flash_block`], whichever the family has - what
/// every flashing path asks.
pub fn project_flash_block(
    csv: &str,
    memory_x: &str,
    store: Option<&FlashStoreConfig>,
    family: &str,
    part: &str,
    runtime: Runtime,
) -> Option<String> {
    if family.starts_with("stm32") {
        stm32_flash_block(memory_x, store, family, part, runtime)
    } else {
        flash_block(csv, store, family)
    }
}

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
        assert_eq!(
            platform("esp32c3", "esp32c3", Runtime::Blocking),
            Ok(Platform::Esp)
        );
        for (f, part) in [
            ("esp32", "esp32"),
            ("esp32s3", "esp32s3"),
            ("esp32c6", "esp32c6"),
            ("rp2040", "rp2040"),
            ("nrf52833", "nrf52833"),
        ] {
            assert!(!supported(f), "{f}");
            assert!(
                platform(f, part, Runtime::Blocking).is_err_and(|r| !r.contains("  ")),
                "{f}"
            );
        }
        assert!(LABEL.len() <= 16);
    }

    /// The table finds a part by any spelling the IDE carries for it, and
    /// knows the facts the store is laid out by.
    #[test]
    fn a_part_is_found_by_its_name_however_it_is_spelled() {
        for name in [
            "stm32f103c8",
            "STM32F103C8",
            "stm32f103c8t6",
            "STM32F103C8Tx",
        ] {
            let (g, flags) = geometry(name).expect(name);
            assert_eq!((g.size, g.page, g.write), (64 * 1024, 1024, 4), "{name}");
            assert_eq!(flags & geo::UNIFORM, geo::UNIFORM);
        }
        assert_eq!(geometry("STM32G431CBUx").map(|g| g.0.page), Some(2048));
        // F071x8: 64 KiB like an F030x8, but 2 KiB pages - a size rule would miss it.
        assert_eq!(geometry("stm32f071c8").map(|g| g.0.page), Some(2048));
        assert_eq!(geometry("stm32f030c8").map(|g| g.0.page), Some(1024));
        assert_eq!(geometry("stm32n657x0"), None, "no internal flash");
        assert_eq!(geometry("stm32xyz"), None);
    }

    /// Which HAL writes the store follows the runtime; which parts get it at
    /// all follows the flash, each refusal in a sentence.
    #[test]
    fn the_platform_follows_the_runtime_and_refuses_what_it_cannot_lay_out() {
        let f1 = |rt| platform("stm32f1", "stm32f103c8t6", rt);
        assert!(matches!(
            f1(Runtime::Blocking),
            Ok(Platform::Stm32 {
                hal: StmHal::F1Hal,
                ..
            })
        ));
        assert!(matches!(
            f1(Runtime::Native),
            Ok(Platform::Stm32 {
                hal: StmHal::F1Hal,
                ..
            })
        ));
        assert!(matches!(
            f1(Runtime::Async),
            Ok(Platform::Stm32 {
                hal: StmHal::Embassy,
                ..
            })
        ));
        assert!(f1(Runtime::Rtic).is_err_and(|r| r.contains("RTIC")));
        let ok = |family: &str, part: &str| {
            matches!(
                platform(family, part, Runtime::Blocking),
                Ok(Platform::Stm32 {
                    hal: StmHal::Embassy,
                    ..
                })
            )
        };
        assert!(ok("stm32g4", "STM32G431CBUx"));
        assert!(ok("stm32wba", "stm32wba52cg"));
        assert!(ok("stm32g0", "stm32g071rb"));
        assert!(ok("stm32u5", "stm32u575zi"));
        let refused = |family: &str, part: &str, why: &str| {
            let r = platform(family, part, Runtime::Blocking).expect_err(part);
            assert!(r.contains(why), "{part}: {r}");
        };
        // Found by review: embassy's L4/WL driver never resets the flash
        // data cache around an erase.
        refused("stm32l4", "stm32l432kc", "data cache");
        refused("stm32wl", "stm32wle5jc", "data cache");
        // F2/F4/F7 end in 128/256 KiB sectors: the store goes right after the
        // vector table instead, in the small ones.
        let after_vectors = |family: &str, part: &str| {
            matches!(
                platform(family, part, Runtime::Blocking),
                Ok(Platform::Stm32 {
                    hal: StmHal::Embassy,
                    layout: Layout::AfterVectors,
                    ..
                })
            )
        };
        assert!(after_vectors("stm32f4", "stm32f411ce"));
        assert!(
            after_vectors("stm32f4", "stm32f401cb"),
            "48 of 128 KiB is fine"
        );
        assert!(after_vectors("stm32f2", "stm32f205rb"));
        assert!(after_vectors("stm32f7", "stm32f746zg"), "32 KiB sectors");
        assert!(after_vectors("stm32f7", "stm32f722ze"));
        assert!(matches!(
            platform("stm32f4", "stm32f411ce", Runtime::Async),
            Ok(Platform::Stm32 {
                layout: Layout::AfterVectors,
                ..
            })
        ));
        // Found by review: uniform 128 KiB sectors passed the size rule on a
        // 2 MB H7 - 256 KiB of store all the same.
        refused("stm32h7", "stm32h743zi", "128 KiB");
        // metapac gives the L552xC one 4 KiB-page bank; the chip ships dual
        // bank with 2 KiB pages, and embassy would erase program flash.
        refused("stm32l5", "stm32l552cc", "option bytes");
        // Small enough a sector, too small a chip: 2 x 16 KiB of 64 KiB.
        refused("stm32f4", "stm32f410c8", "50% of this");
        assert!(ok("stm32f0", "stm32f030f4"), "2 x 1 KiB of 16 KiB is fine");
        refused("stm32l0", "stm32l073rz", "0x00");
        refused("stm32g4", "stm32g474re", "option bytes");
        refused("stm32h7", "stm32h743vg", "gap");
        refused("stm32f1", "stm32f103zg", "XL-density");
        refused("stm32wb", "stm32wb55rg", "radio");
        refused("stm32h7", "stm32h7s3l8", "stub");
        refused("stm32n6", "stm32n657x0", "no internal flash");
    }

    /// An F4 ends in 128 KiB sectors: its store is the 16 KiB sectors right
    /// after the vector table's, the program starting after them.
    #[test]
    fn an_f4_store_sits_after_the_vector_table() {
        let (g, _) = geometry("stm32f411ce").unwrap();
        assert_eq!((g.head_page, g.head_size), (16 * 1024, 64 * 1024));
        let p = Platform::Stm32 {
            hal: StmHal::Embassy,
            geo: g,
            layout: Layout::AfterVectors,
        };
        let c = FlashStoreConfig::default_after_vectors(&g);
        assert_eq!(c.range(), 0x4000..0xC000);
        assert!(c.problems_on(&p).is_empty(), "{:?}", c.problems_on(&p));
        let three = FlashStoreConfig { size: 0xC000, ..c };
        assert!(three.problems_on(&p).is_empty(), "sectors 1 to 3");
        let four = FlashStoreConfig { size: 0x10000, ..c };
        assert!(
            four.problems_on(&p)
                .iter()
                .any(|s| s.contains("first 64 KiB"))
        );
        let moved = FlashStoreConfig {
            offset: 0x8000,
            ..c
        };
        assert!(
            moved
                .problems_on(&p)
                .iter()
                .any(|s| s.contains("right after the vector table"))
        );
        // A hand-written memory.x must start the program after the store.
        let plain = "MEMORY {\n  FLASH : ORIGIN = 0x08000000, LENGTH = 512K\n}\n";
        let after = |t: &str| memory_x_overlap(t, &c, Layout::AfterVectors);
        assert!(after(plain).is_some_and(|s| s.contains("_stext = 0x0800C000;")));
        assert!(
            after(&format!("{plain}_stext = 0x08004000;\n"))
                .is_some_and(|s| s.contains("inside the flash store"))
        );
        assert_eq!(after(&format!("{plain}_stext = 0x08000000 + 48K;\n")), None);
        assert_eq!(
            after(&format!("{plain}PROVIDE(_stext = 0x0800C000);\n")),
            None
        );
        // A symbol cannot be evaluated here - not refused on a guess.
        assert_eq!(after(&format!("{plain}_stext = _flash_store_end;\n")), None);
        let late = "MEMORY { FLASH : ORIGIN = 0x08010000, LENGTH = 448K }\n_stext = 0x08010000;\n";
        assert!(after(late).is_some_and(|s| s.contains("FLASH must start at 0x08000000")));
    }

    /// The STM32 default is two pages at the very end, and only end-of-flash
    /// layouts in whole pages pass.
    #[test]
    fn an_stm32_store_is_whole_pages_at_the_end_of_flash() {
        let (g, _) = geometry("stm32f103c8").unwrap();
        let p = Platform::Stm32 {
            hal: StmHal::F1Hal,
            geo: g,
            layout: Layout::End,
        };
        let c = FlashStoreConfig::default_stm32(&g);
        assert_eq!(c.range(), 0xF800..0x10000);
        assert!(c.problems_on(&p).is_empty(), "{:?}", c.problems_on(&p));
        let one = FlashStoreConfig {
            size: 1024,
            offset: 0xFC00,
            ..c
        };
        assert!(
            one.problems_on(&p)
                .iter()
                .any(|s| s.contains("at least 2 pages"))
        );
        let odd = FlashStoreConfig {
            size: 0x1200,
            offset: 0xEE00,
            ..c
        };
        assert!(
            odd.problems_on(&p)
                .iter()
                .any(|s| s.contains("whole pages"))
        );
        let low = FlashStoreConfig {
            offset: 0x8000,
            ..c
        };
        assert!(
            low.problems_on(&p)
                .iter()
                .any(|s| s.contains("end where the flash ends"))
        );
        // Settings saved on the other platform say so.
        assert!(
            !FlashStoreConfig::default_for("esp32c3")
                .problems_on(&p)
                .is_empty()
        );
        assert!(!c.problems_on(&Platform::Esp).is_empty());
        assert_eq!(
            FlashStoreMode::from_token("memoryx"),
            Some(FlashStoreMode::MemoryX)
        );
    }

    /// Found by review: the Size bar's parser stops at the first space and
    /// takes the first `FLASH*` region, which refused a memory.x reserving the
    /// store exactly (`64K - 2K`) and missed one starting past a bootloader.
    #[test]
    fn memory_x_flash_reads_hand_written_expressions() {
        let read = |t: &str| memory_x_flash(t);
        assert_eq!(
            read("MEMORY {\n  FLASH : ORIGIN = 0x08000000, LENGTH = 64K - 2K\n}\n"),
            Some((0x0800_0000, 62 * 1024))
        );
        assert_eq!(
            read("  FLASH (rx) : ORIGIN = 0x08000000 + 16K, LENGTH = 48K\n"),
            Some((0x0800_4000, 48 * 1024))
        );
        assert_eq!(
            read("MEMORY { FLASH : ORIGIN = 0x08000000, LENGTH = 64K }\n"),
            Some((0x0800_0000, 64 * 1024))
        );
        // Only the region named FLASH counts, wherever it is listed.
        assert_eq!(
            read(
                "  FLASH_STORE : ORIGIN = 0x0800F800, LENGTH = 2K\n  FLASH : ORIGIN = 0x08000000, LENGTH = 62K\n"
            ),
            Some((0x0800_0000, 62 * 1024))
        );
        // Comments are not regions; what cannot be evaluated is not guessed.
        assert_eq!(read("/* FLASH : ORIGIN = 0, LENGTH = 1K */\n"), None);
        assert_eq!(
            read("  FLASH : ORIGIN = ORIGIN(BOOT), LENGTH = 64K\n"),
            None
        );
        // The refusal reads the same way: `64K - 2K` reserves the F103C8's store.
        let (g, _) = geometry("stm32f103c8").unwrap();
        let c = FlashStoreConfig::default_stm32(&g);
        let exact = "  FLASH : ORIGIN = 0x08000000, LENGTH = 64K - 2K\n";
        assert_eq!(memory_x_overlap(exact, &c, Layout::End), None);
        let past_boot = "  FLASH : ORIGIN = 0x08000000 + 16K, LENGTH = 48K\n";
        assert!(memory_x_overlap(past_boot, &c, Layout::End).is_some());
    }

    /// The IDE's memory.x never overlaps; one the user wrote with FLASH up to
    /// the end of the chip is refused before a flash, with the LENGTH to use.
    #[test]
    fn a_memory_x_whose_flash_runs_into_the_store_is_refused() {
        let (g, _) = geometry("stm32f103c8").unwrap();
        let c = FlashStoreConfig::default_stm32(&g);
        let full = "MEMORY\n{\n  FLASH : ORIGIN = 0x08000000, LENGTH = 64K\n  RAM : ORIGIN = 0x20000000, LENGTH = 20K\n}\n";
        let why = stm32_flash_block(full, Some(&c), "stm32f1", "stm32f103c8", Runtime::Blocking)
            .expect("overlap");
        assert!(why.contains("62K"), "{why}");
        let shrunk = full.replace("64K", "62K");
        assert_eq!(
            stm32_flash_block(
                &shrunk,
                Some(&c),
                "stm32f1",
                "stm32f103c8",
                Runtime::Blocking
            ),
            None
        );
        // Off, or on a part it is not generated for: nothing to check.
        assert_eq!(
            stm32_flash_block(full, None, "stm32f1", "stm32f103c8", Runtime::Blocking),
            None
        );
        assert_eq!(
            stm32_flash_block(full, Some(&c), "stm32f1", "stm32f103c8", Runtime::Rtic),
            None
        );
        assert_eq!(
            stm32_reservation(Some(&c), "stm32f1", "stm32f103c8", Runtime::Blocking),
            Some(Reservation {
                range: 0xF800..0x10000,
                layout: Layout::End,
            })
        );
        assert_eq!(
            stm32_reservation(Some(&c), "stm32f1", "stm32f103c8", Runtime::Rtic),
            None
        );
    }

    #[test]
    fn the_mode_token_round_trips() {
        for m in [FlashStoreMode::Partition, FlashStoreMode::Nvs] {
            assert_eq!(FlashStoreMode::from_token(m.token()), Some(m));
        }
        assert_eq!(FlashStoreMode::from_token("bogus"), None);
    }
}
