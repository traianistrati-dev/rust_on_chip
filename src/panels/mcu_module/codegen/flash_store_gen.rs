//! Code for the Configuration tab's flash store: `src/pins/configs/flash_store.rs`,
//! the `main.rs` lines that hand it the flash, and the one line seeded below
//! the markers.
//!
//! ONE body for every runtime and platform. `sequential-storage` 8 is
//! async-only, so the store has `load`/`save` for the Async runtime and
//! `load_blocking` / `save_blocking` (`embassy_futures::block_on` over a
//! `BlockingAsync` that never pends) for Blocking, Native and RTIC. What
//! differs per platform ([`flash_store::Platform`]) is around that body:
//!
//! - **ESP**: esp-storage's `FlashStorage`, and `verify`, which reads the
//!   ESP-IDF partition table on the chip.
//! - **STM32, embassy-stm32**: its blocking `Flash`, and compile-time checks
//!   of the range against embassy's own `FLASH_SIZE` / `MAX_ERASE_SIZE`.
//! - **STM32F1, stm32f1xx-hal** (Blocking, Native): an `F1Flash` adapter that
//!   puts the HAL's `FlashWriter` behind the `NorFlash` traits.
//!
//! On STM32 the reservation is `memory.x` (`project_gen::memory_x_body`), so
//! the checks are the BUILD's: a range the driver cannot erase, or a program
//! that grows into the store, fails to compile or to link.
//!
//! The templates are assembled from shared fragments (`concat!` over the
//! macros below), so the body cannot drift between platforms - and the ESP
//! file stays exactly what it was before the STM32 ones existed.
//!
//! The file lives under `pins/configs/`, not at `src/flash_store.rs`, for what
//! that buys for free: pruning and dependency removal when the toggle goes
//! off, `pub mod` in `configs/mod.rs`, and no dead-code warnings for methods
//! the user has not called yet (a private `mod` warns on every one, measured).
//! A `use` in the generated block makes the requested spelling work anyway:
//! `flash_store::ConfigStore::new(flash)`.

use crate::panels::mcu_module::codegen::GEN_END;
use crate::panels::mcu_module::flash_store::{FlashStoreConfig, Layout, Platform, StmHal};
use crate::panels::mcu_module::mcu::Mcu;

/// The config file's name under `src/pins/configs/`.
pub const FILE: &str = "flash_store.rs";

/// The STM32 glue beside it, generated whole: what differs between
/// embassy-stm32 and stm32f1xx-hal, so the user's `FILE` is the same under
/// both and a runtime switch never has to rewrite it.
pub const HAL_FILE: &str = "flash_store_hal.rs";

// The template fragments. Raw strings in macros, because `concat!` takes
// literals only.
macro_rules! esp_gen {
    () => {
        r#"// <<< GENERATED>>>
// Flash store (from the Configuration tab) — auto-updated; edit it in the tab.
// The bytes the store owns: exactly the {ROW} of the partition table,
// which `verify` below checks on the chip.
pub const STORE_RANGE: core::ops::Range<u32> = 0x{START}..0x{END};
// ESP-IDF `data` subtype of that partition: 0x06 undefined, 0x02 nvs.
pub const STORE_SUBTYPE: u8 = 0x{SUBTYPE};
// <<< GENERATED END >>>
"#
    };
}
macro_rules! esp_intro {
    () => {
        r#"
// Everything below is editable — your changes are preserved on regeneration.
//
// Settings kept in the chip's own flash: `sequential-storage` keeps a small
// key -> value map in STORE_RANGE and spreads the writes over its sectors.
// Put what you want to keep in `Data`, then:
//
//     let mut data = flash_store.load_blocking();      // Async: .load().await
//     data.counter += 1;
//     flash_store.save_blocking(&data).ok();          // Async: .save(&data).await
//
// A save writes flash with interrupts masked for each sector, and an erase
// takes milliseconds: save when something changed, not in a tight loop.

"#
    };
}
macro_rules! esp_imports {
    () => {
        r#"use core::ops::Range;

use embassy_embedded_hal::adapter::BlockingAsync;
use embedded_storage::Storage;
use embedded_storage::nor_flash::NorFlash;
use esp_bootloader_esp_idf::partitions::{self, PARTITION_TABLE_MAX_LEN};
use sequential_storage::cache::{Cache, Uncached};
use sequential_storage::map::{MapConfig, MapStorage, PostcardValue};
use serde::{Deserialize, Serialize};
"#
    };
}
macro_rules! common_a {
    () => {
        r#"
/// Bump it whenever `Data` changes shape: a value stored by another version
/// is ignored, and `load` returns `Data::default()` instead of misreading it.
pub const CONFIG_VERSION: u8 = 1;
const KEY_DATA: u8 = 1;

/// What the store keeps. Add your own fields - anything serde can derive.
#[derive(Serialize, Deserialize, PartialEq, Clone, Debug)]
pub struct Data {
    version: u8,
    pub counter: u32,
}

impl Default for Data {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            counter: 0,
        }
    }
}

impl<'a> PostcardValue<'a> for Data {}

"#
    };
}
macro_rules! esp_buf_doc {
    () => {
        r#"/// The serialized `Data` must fit here (raise it with your fields). Aligned
/// to the flash's 4-byte word.
"#
    };
}
macro_rules! common_a2 {
    () => {
        r#"#[repr(align(4))]
struct Buf([u8; 128]);

type NoCache = Cache<Uncached, Uncached, Uncached, u8>;

"#
    };
}
macro_rules! esp_doc {
    () => {
        r#"/// The store, over any NOR flash - `esp_storage::FlashStorage` here. It owns
/// the flash; call `verify` first if you want the check.
"#
    };
}
macro_rules! common_b {
    () => {
        r#"pub struct ConfigStore<F: NorFlash> {
    map: MapStorage<u8, BlockingAsync<F>, NoCache>,
}

impl<F: NorFlash> ConfigStore<F> {
    pub fn new(flash: F) -> Self {
        Self {
            map: MapStorage::new(
                BlockingAsync::new(flash),
                // Checked while building: a range under two sectors, or off a
                // sector boundary, fails the build instead of every boot.
                const { MapConfig::<BlockingAsync<F>>::new(STORE_RANGE) },
                Cache::new_uncached(),
            ),
        }
    }

    /// The stored `Data`, or `Data::default()` when there is none yet (first
    /// boot), it is of another `CONFIG_VERSION`, or it cannot be read.
    pub async fn load(&mut self) -> Data {
        let mut buf = Buf([0; 128]);
        match self.map.fetch_item::<Data>(&mut buf.0, &KEY_DATA).await {
            Ok(Some(d)) if d.version == CONFIG_VERSION => d,
            _ => Data::default(),
        }
    }

    /// Store `data`. When the range holds something sequential-storage cannot
    /// read - another firmware's data, a store that moved - it is erased once
    /// and the write tried again.
    pub async fn save(&mut self, data: &Data) -> Result<(), sequential_storage::Error<F::Error>> {
        let mut buf = Buf([0; 128]);
        match self.map.store_item(&mut buf.0, &KEY_DATA, data).await {
            Err(sequential_storage::Error::Corrupted { .. }) => {
                self.map.erase_all().await?;
                self.map.store_item(&mut buf.0, &KEY_DATA, data).await
            }
            other => other,
        }
    }

    /// `load` for the blocking runtimes.
    pub fn load_blocking(&mut self) -> Data {
        embassy_futures::block_on(self.load())
    }

    /// `save` for the blocking runtimes.
    pub fn save_blocking(&mut self, data: &Data) -> Result<(), sequential_storage::Error<F::Error>> {
        embassy_futures::block_on(self.save(data))
    }
}
"#
    };
}
macro_rules! esp_tail {
    () => {
        r#"
/// Why `verify` refused.
#[derive(Debug)]
pub enum StoreError {
    /// The partition table could not be read, or failed its MD5 check.
    Table(partitions::Error),
    /// No `data` partition of STORE_SUBTYPE covers exactly STORE_RANGE;
    /// `found` is the first one of that subtype, if any.
    NotReserved { found: Option<Range<u32>> },
    /// The partition is flagged `readonly`. ESP-IDF code would not write it;
    /// esp-storage writes raw flash and ignores the flag, so the store would
    /// overwrite a partition the table marks read-only.
    ReadOnly,
    /// STORE_RANGE ends past the flash size the bootloader header states.
    BeyondFlash { capacity: u32 },
}

/// Is STORE_RANGE really reserved on THIS chip? Reads the partition table the
/// bootloader uses and looks for a `data` partition of STORE_SUBTYPE with
/// exactly that range. Call it before `ConfigStore::new`, which takes `flash`:
///
///     flash_store::verify(&mut flash).expect("flash store not reserved");
///
/// The lookup is raw on purpose: esp-bootloader-esp-idf's `find_partition`
/// panics on a subtype it has no name for, and a table may hold one.
pub fn verify<F: Storage>(flash: &mut F) -> Result<(), StoreError> {
    let capacity = flash.capacity() as u32;
    if capacity != 0 && STORE_RANGE.end > capacity {
        return Err(StoreError::BeyondFlash { capacity });
    }
    let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];
    let table = partitions::read_partition_table(flash, &mut buf).map_err(StoreError::Table)?;
    let mut found = None;
    for e in table.iter() {
        if e.raw_type() != 0x01 || e.raw_subtype() != STORE_SUBTYPE {
            continue;
        }
        let range = e.offset()..e.offset() + e.len();
        if range == STORE_RANGE {
            return if e.flags() & PART_FLAG_READONLY != 0 {
                Err(StoreError::ReadOnly)
            } else {
                Ok(())
            };
        }
        found.get_or_insert(range);
    }
    Err(StoreError::NotReserved { found })
}

/// ESP-IDF's `readonly` flag, bit 1 - what espflash writes. Not
/// esp-bootloader-esp-idf 0.5's `is_read_only()`: that one tests bit 0,
/// which is `encrypted`.
const PART_FLAG_READONLY: u32 = 1 << 1;
"#
    };
}
macro_rules! stm_gen {
    () => {
        r#"// <<< GENERATED>>>
// Flash store (from the Configuration tab) — auto-updated; edit it in the tab.
// The bytes the store owns, as offsets from the start of flash (0x08000000):
// {WHERE}.
pub const STORE_RANGE: core::ops::Range<u32> = 0x{START}..0x{END};
// <<< GENERATED END >>>
"#
    };
}
macro_rules! stm_intro {
    () => {
        r#"
// Everything below is editable — your changes are preserved on regeneration.
//
// Settings kept in the chip's own flash: `sequential-storage` keeps a small
// key -> value map in STORE_RANGE and spreads the writes over its pages.
// Put what you want to keep in `Data`, then:
//
//     let mut data = flash_store.load_blocking();      // Async: .load().await
//     data.counter += 1;
//     flash_store.save_blocking(&data).ok();          // Async: .save(&data).await
//
// A page erase takes milliseconds, and the CPU waits while flash is written
// when the code runs from the same bank: save when something changed, not in a
// tight loop. Flashing from the IDE keeps these pages; a chip erase wipes them.

"#
    };
}
macro_rules! stm_imports {
    () => {
        r#"use embassy_embedded_hal::adapter::BlockingAsync;
use embedded_storage::nor_flash::NorFlash;
use sequential_storage::cache::{Cache, Uncached};
use sequential_storage::map::{MapConfig, MapStorage, PostcardValue};
use serde::{Deserialize, Serialize};
"#
    };
}
macro_rules! stm_buf_doc {
    () => {
        r#"/// The serialized `Data` must fit here (raise it with your fields).
"#
    };
}
macro_rules! stm_doc {
    () => {
        r#"/// The store, over any NOR flash - the chip's own here, as the generated
/// block in main.rs hands it over (see flash_store_hal.rs). It owns the flash.
"#
    };
}
macro_rules! hal_embassy {
    () => {
        r#"// <<< GENERATED>>>
// Flash store glue for embassy-stm32 (from the Configuration tab) — regenerated
// whole, with the runtime; your code goes in flash_store.rs.
//
// Checked while building, against embassy-stm32's own facts about this chip:
// the store ends where the flash ends and starts on an erase unit, so
// STORE_RANGE, memory.x and the flash driver cannot disagree.
use super::flash_store::STORE_RANGE;

const _: () = {
    use embassy_stm32::flash::{FLASH_SIZE, MAX_ERASE_SIZE};
    assert!(
        STORE_RANGE.end as usize == FLASH_SIZE,
        "STORE_RANGE must end where the flash ends (embassy-stm32's FLASH_SIZE)"
    );
    assert!(
        STORE_RANGE.start as usize % MAX_ERASE_SIZE == 0,
        "STORE_RANGE must start on an erase unit (embassy-stm32's MAX_ERASE_SIZE)"
    );
};
// <<< GENERATED END >>>
"#
    };
}
macro_rules! hal_embassy_head {
    () => {
        r#"// <<< GENERATED>>>
// Flash store glue for embassy-stm32 (from the Configuration tab) — regenerated
// whole, with the runtime; your code goes in flash_store.rs.
//
// This flash ends in big sectors, so the store lives in the small ones at its
// start, right after the vector table's (embassy's first flash region). Checked
// while building, against embassy-stm32's own facts about this chip.
use super::flash_store::STORE_RANGE;

const _: () = {
    use embassy_stm32::flash::BANK1_REGION1;
    assert!(
        BANK1_REGION1.offset == 0,
        "the first flash region must start the flash (embassy-stm32's BANK1_REGION1)"
    );
    assert!(
        STORE_RANGE.start == BANK1_REGION1.erase_size,
        "STORE_RANGE must start right after the vector table's sector"
    );
    assert!(
        STORE_RANGE.end <= BANK1_REGION1.size && STORE_RANGE.end % BANK1_REGION1.erase_size == 0,
        "STORE_RANGE must be whole sectors of the first flash region"
    );
};
// <<< GENERATED END >>>
"#
    };
}
macro_rules! hal_f1 {
    () => {
        r#"// <<< GENERATED>>>
// Flash store glue for stm32f1xx-hal (from the Configuration tab) — regenerated
// whole, with the runtime; your code goes in flash_store.rs.
//
// stm32f1xx-hal has no `NorFlash` of its own: `F1Flash` puts its flash writer
// behind the traits sequential-storage needs.
use embedded_storage::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};
use stm32f1xx_hal::flash::{self, FlashSize, SectorSize};

use super::flash_store::STORE_RANGE;

/// This F1's page and flash size in KiB, for stm32f1xx-hal's flash writer.
pub const PAGE_KIB: u32 = {PAGE_KIB};
pub const FLASH_KIB: u32 = {FLASH_KIB};

/// stm32f1xx-hal's flash behind the `NorFlash` traits. It owns `flash::Parts` -
/// what `dp.FLASH.constrain()` returned, once the clocks are frozen - and
/// opens a writer per call.
///
/// Words of 4 bytes, two of the F1's half-words, as embassy-stm32 writes them
/// on the Async runtime: the stored data reads the same under either HAL.
pub struct F1Flash {
    parts: flash::Parts,
}

impl F1Flash {
    pub fn new(parts: flash::Parts) -> Self {
        Self { parts }
    }

    fn writer(&mut self) -> flash::FlashWriter<'_> {
        self.parts.writer(PAGE, SIZE)
    }
}

const PAGE: SectorSize = match PAGE_KIB {
    1 => SectorSize::Sz1K,
    2 => SectorSize::Sz2K,
    _ => panic!("PAGE_KIB must be 1 or 2 on an STM32F1"),
};

const SIZE: FlashSize = match FLASH_KIB {
    16 => FlashSize::Sz16K,
    32 => FlashSize::Sz32K,
    64 => FlashSize::Sz64K,
    128 => FlashSize::Sz128K,
    256 => FlashSize::Sz256K,
    384 => FlashSize::Sz384K,
    512 => FlashSize::Sz512K,
    _ => panic!("FLASH_KIB is not the flash size of an STM32F1 the store supports"),
};

// The store ends where the flash ends, on a page - checked while building.
const _: () = assert!(
    STORE_RANGE.end == FLASH_KIB * 1024 && STORE_RANGE.start % (PAGE_KIB * 1024) == 0,
    "STORE_RANGE must be whole pages at the end of flash"
);

/// stm32f1xx-hal's flash error, as the `NorFlash` traits report it.
#[derive(Debug)]
pub struct F1FlashError(pub flash::Error);

impl NorFlashError for F1FlashError {
    fn kind(&self) -> NorFlashErrorKind {
        match self.0 {
            flash::Error::AddressMisaligned | flash::Error::LengthNotMultiple2 => {
                NorFlashErrorKind::NotAligned
            }
            flash::Error::AddressLargerThanFlash | flash::Error::LengthTooLong => {
                NorFlashErrorKind::OutOfBounds
            }
            _ => NorFlashErrorKind::Other,
        }
    }
}

impl ErrorType for F1Flash {
    type Error = F1FlashError;
}

impl ReadNorFlash for F1Flash {
    // stm32f1xx-hal reads from even offsets only.
    const READ_SIZE: usize = 2;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let writer = self.writer();
        let data = writer.read(offset, bytes.len()).map_err(F1FlashError)?;
        bytes.copy_from_slice(data);
        Ok(())
    }

    fn capacity(&self) -> usize {
        FLASH_KIB as usize * 1024
    }
}

impl NorFlash for F1Flash {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = PAGE_KIB as usize * 1024;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.writer()
            .erase(from, (to - from) as usize)
            .map_err(F1FlashError)
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.writer().write(offset, bytes).map_err(F1FlashError)
    }
}
// <<< GENERATED END >>>
"#
    };
}

/// The ESP template. Only the two constants sit inside the markers; everything
/// below them is the user's, kept across regeneration and never force-rewritten
/// (`project_tree::logic::sync_config_files`), since no runtime changes it.
const TMPL: &str = concat!(
    esp_gen!(),
    esp_intro!(),
    esp_imports!(),
    common_a!(),
    esp_buf_doc!(),
    common_a2!(),
    esp_doc!(),
    common_b!(),
    esp_tail!()
);

/// The STM32 template - the same whichever HAL writes the flash, so a runtime
/// switch (the F1 between stm32f1xx-hal and embassy-stm32) never touches the
/// user's file: what differs lives in [`HAL_FILE`].
const TMPL_STM32: &str = concat!(
    stm_gen!(),
    stm_intro!(),
    stm_imports!(),
    common_a!(),
    stm_buf_doc!(),
    common_a2!(),
    stm_doc!(),
    common_b!()
);

/// The STM32 glue on embassy-stm32 (every family; the F1 on Async): the
/// build-time checks. Generated whole.
const HAL_EMBASSY: &str = hal_embassy!();

/// The F2/F4/F7 glue on embassy-stm32 when the store sits after the vector
/// table: the build-time checks against `BANK1_REGION1`. Generated whole.
const HAL_EMBASSY_HEAD: &str = hal_embassy_head!();

/// The STM32F1 glue on stm32f1xx-hal (Blocking, Native): the `F1Flash`
/// adapter. Generated whole.
const HAL_F1: &str = hal_f1!();

/// Every template of the USER's file, for [`is_pristine`].
const TEMPLATES: [&str; 2] = [TMPL, TMPL_STM32];

/// The store's platform on `mcu`'s chip and runtime, `None` where it is not
/// generated.
fn platform_of(mcu: &Mcu) -> Option<Platform> {
    crate::panels::mcu_module::flash_store::platform_of(mcu).ok()
}

/// The store's files for `mcu`, or nothing when it is off or not generated
/// for the chip and runtime.
pub fn config_files_for(mcu: &Mcu) -> Vec<(String, String)> {
    config_files(mcu.flash_store.as_ref(), platform_of(mcu))
}

/// The store's files for `cfg` on `platform`, or nothing when the store is
/// off or not generated (`platform` is `None`): `flash_store.rs` - the user's,
/// below its markers - and on an STM32 [`HAL_FILE`] too, generated whole.
pub fn config_files(
    cfg: Option<&FlashStoreConfig>,
    platform: Option<Platform>,
) -> Vec<(String, String)> {
    let (Some(cfg), Some(platform)) = (cfg, platform) else {
        return Vec::new();
    };
    let range = cfg.range();
    let fill = |t: &str| {
        t.replace("{START}", &format!("{:X}", range.start))
            .replace("{END}", &format!("{:X}", range.end))
    };
    match platform {
        Platform::Esp => {
            let row = if cfg.needs_partition_table() {
                "`flash_store` row"
            } else {
                "default `nvs` partition"
            };
            let body = TMPL
                .replace("{ROW}", row)
                .replace("{SUBTYPE}", &format!("{:02X}", cfg.subtype()));
            vec![(FILE.to_owned(), fill(&body))]
        }
        Platform::Stm32 { hal, geo, layout } => {
            let pages = cfg.size / geo.store_page(layout).max(1);
            let place = match layout {
                Layout::End => format!("the last {pages} pages, which memory.x keeps out of FLASH"),
                Layout::AfterVectors => format!(
                    "{pages} sectors right after the vector table, before the program (_stext)"
                ),
            };
            let body = TMPL_STM32.replace("{WHERE}", &place);
            let glue = match (hal, layout) {
                (StmHal::Embassy, Layout::End) => HAL_EMBASSY.to_owned(),
                (StmHal::Embassy, Layout::AfterVectors) => HAL_EMBASSY_HEAD.to_owned(),
                (StmHal::F1Hal, _) => HAL_F1
                    .replace("{PAGE_KIB}", &(geo.page / 1024).to_string())
                    .replace("{FLASH_KIB}", &(geo.size / 1024).to_string()),
            };
            vec![(FILE.to_owned(), fill(&body)), (HAL_FILE.to_owned(), glue)]
        }
    }
}

/// Does this set of config files hold the store? THE dependency decision,
/// shared by the app and the ESP harness so the two cannot build different
/// manifests: the crates are needed exactly when the file is generated.
pub fn in_files(files: &[(String, String)]) -> bool {
    files.iter().any(|(name, _)| name == FILE)
}

/// The store file's path in the project tree.
pub fn tree_path() -> String {
    format!("src/pins/configs/{FILE}")
}

/// The `pins/configs/` paths the tree must keep although `files` no longer
/// generates them: the store's file, when it is switched off AND the user
/// wrote in it. A pristine one goes like any pruned config file.
pub fn kept_paths(files: &[(String, String)], tree: &[(String, String)]) -> Vec<String> {
    if in_files(files) {
        return Vec::new();
    }
    let path = tree_path();
    tree.iter()
        .filter(|(p, content)| *p == path && !is_pristine(content))
        .map(|(p, _)| p.clone())
        .collect()
}

/// Is `content` still exactly a template below its markers - any platform's?
/// Then switching the store off may let it go; otherwise it holds the user's
/// `Data` and is kept (see `ProjectTreeState::kept_config_files`).
pub fn is_pristine(content: &str) -> bool {
    let tail = |s: &str| {
        s.split_once(GEN_END_CFG)
            .map(|(_, t)| t.replace("\r\n", "\n"))
    };
    let Some(mine) = tail(content) else {
        return false;
    };
    TEMPLATES.iter().any(|t| tail(t).is_some_and(|t| t == mine))
}

/// The end marker of a config file's GENERATED block (not `main.rs`'s).
const GEN_END_CFG: &str = "// <<< GENERATED END >>>";

/// The generated-block lines in `main.rs` for `mcu`'s store - see
/// [`init_lines`].
pub fn init_lines_for(mcu: &Mcu) -> String {
    init_lines(mcu.flash_store.as_ref(), platform_of(mcu))
}

/// The generated-block lines in `main.rs`: the module in scope, and the flash
/// handed over as `flash`. Empty when the store is off or not generated.
///
/// - ESP: `esp_storage::FlashStorage::new(peripherals.FLASH)`.
/// - embassy-stm32: `Flash::new_blocking(p.FLASH)` - nothing else takes
///   `p.FLASH` there.
/// - stm32f1xx-hal: `flash` is already `dp.FLASH.constrain()`, whose `acr`
///   froze the clocks a few lines up; the adapter takes it over. These lines
///   land after the clocks, in the slot the Custom modules use.
///
/// `mut` and the `allow`s keep a project that has not touched the store yet
/// warning-free, while `flash_store::verify(&mut flash)` still works on an
/// ESP. The binding is `flash`, not `p…`/`gpio…`, which `parse_main_rs` reads
/// as pins.
pub fn init_lines(cfg: Option<&FlashStoreConfig>, platform: Option<Platform>) -> String {
    let (Some(_), Some(platform)) = (cfg, platform) else {
        return String::new();
    };
    let handover = match platform {
        Platform::Esp => "    let mut flash = esp_storage::FlashStorage::new(peripherals.FLASH);\n",
        Platform::Stm32 {
            hal: StmHal::Embassy,
            layout: Layout::End,
            ..
        } => "    let mut flash = embassy_stm32::flash::Flash::new_blocking(p.FLASH);\n",
        // F2/F4/F7: the first region alone, whose sectors are the small ones.
        Platform::Stm32 {
            hal: StmHal::Embassy,
            layout: Layout::AfterVectors,
            ..
        } => concat!(
            "    let mut flash = embassy_stm32::flash::Flash::new_blocking(p.FLASH)\n",
            "        .into_blocking_regions()\n",
            "        .bank1_region1;\n",
        ),
        Platform::Stm32 {
            hal: StmHal::F1Hal, ..
        } => "    let mut flash = crate::pins::configs::flash_store_hal::F1Flash::new(flash);\n",
    };
    format!(
        concat!(
            "\n    // ── Flash store ──\n",
            "    #[allow(unused_imports)]\n",
            "    use crate::pins::configs::flash_store;\n",
            "    #[allow(unused_mut, unused_variables)]\n",
            "{}",
        ),
        handover
    )
}

/// The line seeded at the head of the user's tail, with the blank line after.
pub const TAIL_SEED: &str = concat!(
    "    #[allow(unused_mut, unused_variables)]\n",
    "    let mut flash_store = flash_store::ConfigStore::new(flash);\n",
    "\n",
);

/// Put [`TAIL_SEED`] at the head of the tail when the store is on - but ONLY
/// while the tail still starts with the untouched loop seed. A loop the user
/// wrote in is theirs, and the card shows the line to copy instead.
///
/// Off, the line goes whatever follows it: the generated block no longer
/// binds `flash` nor brings `flash_store` into scope, so it could only fail
/// to compile.
///
/// The runtime's seed swap (`common::retarget_pristine_tail`) cannot see a
/// seed behind this line, so it is redone here: the line is lifted off, the
/// seed retargeted, the line put back.
pub fn seed_tail(code: String, enabled: bool, want_async: bool) -> String {
    use super::common::{ASYNC_USER_TAIL, USER_TAIL, retarget_pristine_tail};
    let Some((cut, lead, body)) = split_tail(&code) else {
        return code;
    };
    let seeded = |s: &str| s.starts_with(USER_TAIL) || s.starts_with(ASYNC_USER_TAIL);
    let lifted = body.strip_prefix(TAIL_SEED);
    let rest = lifted.unwrap_or(body);
    let new_body = if seeded(rest) {
        let rest = retarget_pristine_tail(rest, want_async);
        if enabled {
            format!("{TAIL_SEED}{rest}")
        } else {
            rest
        }
    } else if lifted.is_some() && !enabled {
        rest.to_owned()
    } else {
        return code;
    };
    if new_body == body {
        return code;
    }
    format!("{}{lead}{new_body}", &code[..cut])
}

/// Take [`TAIL_SEED`] off the head of the tail and touch nothing else - for a
/// `main.rs` whose chip no longer generates the store (an edited `mcu=`
/// marker), where `seed_tail`'s runtime swap must not run on a foreign seed.
pub fn strip_tail_seed(code: String) -> String {
    let Some((cut, lead, body)) = split_tail(&code) else {
        return code;
    };
    match body.strip_prefix(TAIL_SEED) {
        Some(rest) => format!("{}{lead}{rest}", &code[..cut]),
        None => code,
    }
}

/// `main.rs` split after its GENERATED block: the cut, the blank lines that
/// follow it, and the tail behind them.
fn split_tail(code: &str) -> Option<(usize, &str, &str)> {
    let cut = code.find(GEN_END)? + GEN_END.len();
    let after = &code[cut..];
    let body = after.trim_start_matches('\n');
    Some((cut, &after[..after.len() - body.len()], body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::codegen::common::{ASYNC_USER_TAIL, USER_TAIL};
    use crate::panels::mcu_module::flash_store::FlashStoreMode;

    const ESP: Option<Platform> = Some(Platform::Esp);

    fn on() -> FlashStoreConfig {
        FlashStoreConfig::default_for("esp32c3")
    }

    #[test]
    fn the_file_carries_only_the_range_and_subtype_in_its_block() {
        let files = config_files(Some(&on()), ESP);
        let [(name, body)] = files.as_slice() else {
            panic!("{files:?}");
        };
        assert_eq!(name, FILE);
        assert!(
            body.contains("STORE_RANGE: core::ops::Range<u32> = 0x3FC000..0x400000;"),
            "{body}"
        );
        assert!(body.contains("STORE_SUBTYPE: u8 = 0x06;"), "{body}");
        for p in ["{ROW}", "{START}", "{END}", "{SUBTYPE}"] {
            assert!(!body.contains(p), "{p} survived");
        }
        let block = body.split(GEN_END_CFG).next().unwrap_or("");
        assert!(
            !block.contains("fn "),
            "only constants in the block:\n{block}"
        );
        // The nvs mode points at the default table's partition.
        let nvs = FlashStoreConfig {
            mode: FlashStoreMode::Nvs,
            ..on()
        };
        let body = &config_files(Some(&nvs), ESP)[0].1;
        assert!(body.contains("0x9000..0xF000"), "{body}");
        assert!(body.contains("STORE_SUBTYPE: u8 = 0x02;"), "{body}");
        // Off, or on a chip it is not generated for: nothing.
        assert!(config_files(None, ESP).is_empty());
        assert!(config_files(Some(&on()), None).is_empty());
        assert!(init_lines(Some(&on()), None).is_empty());
    }

    #[test]
    fn a_fresh_template_is_pristine_and_an_edited_one_is_not() {
        let body = config_files(Some(&on()), ESP).remove(0).1;
        assert!(is_pristine(&body));
        // The constants moving does not make it the user's.
        let moved = body.replace("0x3FC000..0x400000", "0x3F0000..0x3F4000");
        assert!(is_pristine(&moved));
        let edited = body.replace("pub counter: u32,", "pub counter: u32,\n    pub level: u8,");
        assert!(!is_pristine(&edited));
        assert!(is_pristine(&body.replace('\n', "\r\n")));
    }

    fn file_with(tail: &str) -> String {
        format!("head\n{GEN_END}\n\n{tail}")
    }

    /// Seeded on a pristine tail of either runtime, removed again, and never
    /// put into a loop the user wrote in.
    #[test]
    fn the_store_line_follows_the_toggle_only_on_an_untouched_tail() {
        for (seed, want_async) in [(USER_TAIL, false), (ASYNC_USER_TAIL, true)] {
            let fresh = file_with(seed);
            let on = seed_tail(fresh.clone(), true, want_async);
            assert!(
                on.contains(&format!("{GEN_END}\n\n{TAIL_SEED}{seed}")),
                "{on}"
            );
            assert_eq!(seed_tail(on.clone(), true, want_async), on, "idempotent");
            assert_eq!(seed_tail(on, false, want_async), fresh, "and back off");
        }
        let mine = file_with("    loop {\n        led.toggle();\n    }\n}\n");
        assert_eq!(seed_tail(mine.clone(), true, false), mine);
        // Seeded, then the loop edited: on keeps it, off still takes the
        // line away (it names a `flash` the block no longer binds).
        let edited = file_with(&format!(
            "{TAIL_SEED}    loop {{\n        led.toggle();\n    }}\n}}\n"
        ));
        assert_eq!(seed_tail(edited.clone(), true, false), edited);
        assert_eq!(seed_tail(edited.clone(), false, false), mine);
        // A chip that never generates the store: only the line goes.
        assert_eq!(strip_tail_seed(edited), mine);
        let foreign = file_with(USER_TAIL);
        assert_eq!(strip_tail_seed(foreign.clone()), foreign);
        // Something AFTER an untouched loop (a module model, a helper) does not
        // stop the seed - the loop itself is still ours.
        let with_model = file_with(&format!("{USER_TAIL}\nmod _usart_1 {{}}\n"));
        assert!(seed_tail(with_model, true, false).contains(TAIL_SEED));
    }

    /// A runtime switch still swaps the loop seed under the store line.
    #[test]
    fn the_seed_under_the_store_line_follows_the_runtime() {
        let blocking_on = seed_tail(file_with(USER_TAIL), true, false);
        let async_on = seed_tail(blocking_on, true, true);
        assert!(
            async_on.contains(&format!("{TAIL_SEED}{ASYNC_USER_TAIL}")),
            "{async_on}"
        );
        let back = seed_tail(async_on, true, false);
        assert!(back.contains(&format!("{TAIL_SEED}{USER_TAIL}")), "{back}");
    }

    #[test]
    fn the_generated_lines_hand_over_the_flash_and_the_module() {
        let lines = init_lines(Some(&on()), ESP);
        assert!(
            lines.contains("let mut flash = esp_storage::FlashStorage::new(peripherals.FLASH);")
        );
        assert!(lines.contains("use crate::pins::configs::flash_store;"));
        assert!(TAIL_SEED.contains("flash_store::ConfigStore::new(flash)"));
        assert!(init_lines(None, ESP).is_empty());
    }

    fn stm32(part: &str, hal: StmHal) -> (FlashStoreConfig, Option<Platform>) {
        use crate::panels::mcu_module::flash_store::geometry;
        let (geo, _) = geometry(part).expect("a metapac part");
        (
            FlashStoreConfig::default_stm32(&geo),
            Some(Platform::Stm32 {
                hal,
                geo,
                layout: Layout::End,
            }),
        )
    }

    /// An F103C8 on its own HAL: the user's file holds the range alone, and
    /// the glue file carries the page, the flash size and the adapter.
    #[test]
    fn the_f1_glue_carries_the_page_and_the_adapter() {
        let (cfg, platform) = stm32("stm32f103c8", StmHal::F1Hal);
        let files = config_files(Some(&cfg), platform);
        let [(name, body), (glue_name, glue)] = files.as_slice() else {
            panic!("{files:?}");
        };
        assert_eq!((name.as_str(), glue_name.as_str()), (FILE, HAL_FILE));
        let (block, rest) = body.split_once(GEN_END_CFG).expect("markers");
        assert!(
            block.contains("STORE_RANGE: core::ops::Range<u32> = 0xF800..0x10000;"),
            "{block}"
        );
        assert!(block.contains("the last 2 pages"), "{block}");
        assert!(!block.contains("fn "), "only constants in the block");
        assert!(
            !rest.contains("F1Flash") && !rest.contains("esp_"),
            "{rest}"
        );
        assert!(glue.contains("pub const PAGE_KIB: u32 = 1;"), "{glue}");
        assert!(glue.contains("pub const FLASH_KIB: u32 = 64;"), "{glue}");
        assert!(glue.contains("impl NorFlash for F1Flash"));
        assert!(glue.trim_end().ends_with(GEN_END_CFG), "generated whole");
        for p in ["{START}", "{END}", "{PAGES}", "{PAGE_KIB}", "{FLASH_KIB}"] {
            assert!(!body.contains(p) && !glue.contains(p), "{p} survived");
        }
        assert!(is_pristine(body));
        let lines = init_lines(Some(&cfg), platform);
        assert!(lines.contains(
            "let mut flash = crate::pins::configs::flash_store_hal::F1Flash::new(flash);"
        ));
    }

    /// Everything on embassy-stm32: its blocking `Flash`, and the range
    /// checked against embassy's own constants while building - in the glue.
    #[test]
    fn the_embassy_glue_checks_embassys_flash_constants() {
        let (cfg, platform) = stm32("stm32g431cb", StmHal::Embassy);
        let files = config_files(Some(&cfg), platform);
        let [(_, body), (_, glue)] = files.as_slice() else {
            panic!("{files:?}");
        };
        assert!(body.contains("0x1F000..0x20000;"), "{body}");
        assert!(glue.contains("STORE_RANGE.end as usize == FLASH_SIZE"));
        assert!(glue.contains("STORE_RANGE.start as usize % MAX_ERASE_SIZE == 0"));
        assert!(!glue.contains("F1Flash"));
        assert!(is_pristine(body));
        let lines = init_lines(Some(&cfg), platform);
        assert!(
            lines.contains("let mut flash = embassy_stm32::flash::Flash::new_blocking(p.FLASH);")
        );
    }

    /// Found by review: the user's file used to differ per HAL, and a runtime
    /// switch never rewrites it - an F1 going Blocking -> Async kept the
    /// stm32f1xx-hal adapter under an embassy manifest. Now only the glue,
    /// generated whole, follows the HAL.
    #[test]
    fn the_users_file_is_the_same_whichever_hal_writes_the_flash() {
        let (cfg, f1) = stm32("stm32f103c8", StmHal::F1Hal);
        let (_, embassy) = stm32("stm32f103c8", StmHal::Embassy);
        let a = config_files(Some(&cfg), f1);
        let b = config_files(Some(&cfg), embassy);
        assert_eq!(a[0], b[0], "flash_store.rs must not depend on the HAL");
        assert_ne!(a[1], b[1], "the glue does");
    }

    /// The ESP file is assembled from the same fragments as the STM32 one,
    /// and must still be exactly what it was before they existed - an older
    /// pristine file must stay pristine, and the store's body the same on all.
    #[test]
    fn every_template_shares_one_body_and_the_esp_one_did_not_move() {
        for t in TEMPLATES {
            assert!(t.contains(common_a!()) && t.contains(common_a2!()));
            assert!(t.contains(common_b!()));
        }
        assert!(
            TMPL.starts_with("// <<< GENERATED>>>\n// Flash store (from the Configuration tab)")
        );
        assert!(
            TMPL.contains(
                "use esp_bootloader_esp_idf::partitions::{self, PARTITION_TABLE_MAX_LEN};"
            )
        );
        assert!(TMPL.ends_with("const PART_FLAG_READONLY: u32 = 1 << 1;\n"));
        assert_eq!(TMPL.len(), 6556, "the ESP template's text changed");
    }
}
