//! Code for the Configuration tab's flash store: `src/pins/configs/flash_store.rs`,
//! the `main.rs` lines that hand it the flash, and the one line seeded below
//! the markers.
//!
//! ONE template for every ESP runtime. `sequential-storage` 8 is async-only,
//! so the store has `load`/`save` for the Async runtime and `load_blocking` /
//! `save_blocking` (`embassy_futures::block_on` over a `BlockingAsync` that
//! never pends) for Blocking, Native and RTIC - which all fall back to the
//! blocking ESP backend. Both shapes were compiled for the ESP32-C3 with no
//! warning, the tail using the store and not using it alike.
//!
//! The file lives under `pins/configs/`, not at `src/flash_store.rs`, for what
//! that buys for free: pruning and dependency removal when the toggle goes
//! off, `pub mod` in `configs/mod.rs`, and no dead-code warnings for methods
//! the user has not called yet (a private `mod` warns on every one, measured).
//! A `use` in the generated block makes the requested spelling work anyway:
//! `flash_store::ConfigStore::new(flash)`.

use crate::panels::mcu_module::codegen::GEN_END;
use crate::panels::mcu_module::flash_store::{self, FlashStoreConfig};

/// The config file's name under `src/pins/configs/`.
pub const FILE: &str = "flash_store.rs";

/// The template. Only the two constants sit inside the markers; everything
/// below them is the user's, kept across regeneration and never force-rewritten
/// (`project_tree::logic::sync_config_files`), since no runtime changes it.
const TMPL: &str = r#"// <<< GENERATED>>>
// Flash store (from the Configuration tab) — auto-updated; edit it in the tab.
// The bytes the store owns: exactly the {ROW} of the partition table,
// which `verify` below checks on the chip.
pub const STORE_RANGE: core::ops::Range<u32> = 0x{START}..0x{END};
// ESP-IDF `data` subtype of that partition: 0x06 undefined, 0x02 nvs.
pub const STORE_SUBTYPE: u8 = 0x{SUBTYPE};
// <<< GENERATED END >>>

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

use core::ops::Range;

use embassy_embedded_hal::adapter::BlockingAsync;
use embedded_storage::Storage;
use embedded_storage::nor_flash::NorFlash;
use esp_bootloader_esp_idf::partitions::{self, PARTITION_TABLE_MAX_LEN};
use sequential_storage::cache::{Cache, Uncached};
use sequential_storage::map::{MapConfig, MapStorage, PostcardValue};
use serde::{Deserialize, Serialize};

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

/// The serialized `Data` must fit here (raise it with your fields). Aligned
/// to the flash's 4-byte word.
#[repr(align(4))]
struct Buf([u8; 128]);

type NoCache = Cache<Uncached, Uncached, Uncached, u8>;

/// The store, over any NOR flash - `esp_storage::FlashStorage` here. It owns
/// the flash; call `verify` first if you want the check.
pub struct ConfigStore<F: NorFlash> {
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
"#;

/// `flash_store.rs` for `cfg`, or nothing when the store is off or not
/// generated for `family`.
pub fn config_files(cfg: Option<&FlashStoreConfig>, family: &str) -> Vec<(String, String)> {
    let Some(cfg) = cfg.filter(|_| flash_store::supported(family)) else {
        return Vec::new();
    };
    let range = cfg.range();
    let row = if cfg.needs_partition_table() {
        "`flash_store` row"
    } else {
        "default `nvs` partition"
    };
    let body = TMPL
        .replace("{ROW}", row)
        .replace("{START}", &format!("{:X}", range.start))
        .replace("{END}", &format!("{:X}", range.end))
        .replace("{SUBTYPE}", &format!("{:02X}", cfg.subtype()));
    vec![(FILE.to_owned(), body)]
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

/// Is `content` still exactly the template below its markers? Then switching
/// the store off may let it go; otherwise it holds the user's `Data` and is
/// kept (see `ProjectTreeState::kept_config_files`).
pub fn is_pristine(content: &str) -> bool {
    let tail = |s: &str| {
        s.split_once(GEN_END_CFG)
            .map(|(_, t)| t.replace("\r\n", "\n"))
    };
    match (tail(content), tail(TMPL)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// The end marker of a config file's GENERATED block (not `main.rs`'s).
const GEN_END_CFG: &str = "// <<< GENERATED END >>>";

/// The generated-block lines in `main.rs`: the module in scope, and the flash
/// handed over. Empty when the store is off or not generated for `family`.
///
/// `mut` and the `allow`s keep a project that has not touched the store yet
/// warning-free, while `flash_store::verify(&mut flash)` still works. The
/// binding is `flash`, which nothing else on ESP takes, and not `p…`/`gpio…`,
/// which `parse_main_rs` reads as pins.
pub fn init_lines(cfg: Option<&FlashStoreConfig>, family: &str) -> String {
    if cfg.is_none() || !flash_store::supported(family) {
        return String::new();
    }
    concat!(
        "\n    // ── Flash store ──\n",
        "    #[allow(unused_imports)]\n",
        "    use crate::pins::configs::flash_store;\n",
        "    #[allow(unused_mut, unused_variables)]\n",
        "    let mut flash = esp_storage::FlashStorage::new(peripherals.FLASH);\n",
    )
    .to_owned()
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

    fn on() -> FlashStoreConfig {
        FlashStoreConfig::default_for("esp32c3")
    }

    #[test]
    fn the_file_carries_only_the_range_and_subtype_in_its_block() {
        let files = config_files(Some(&on()), "esp32c3");
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
        let body = &config_files(Some(&nvs), "esp32c3")[0].1;
        assert!(body.contains("0x9000..0xF000"), "{body}");
        assert!(body.contains("STORE_SUBTYPE: u8 = 0x02;"), "{body}");
        // Off, or on a chip it is not generated for: nothing.
        assert!(config_files(None, "esp32c3").is_empty());
        assert!(config_files(Some(&on()), "esp32c6").is_empty());
        assert!(init_lines(Some(&on()), "stm32f1").is_empty());
    }

    #[test]
    fn a_fresh_template_is_pristine_and_an_edited_one_is_not() {
        let body = config_files(Some(&on()), "esp32c3").remove(0).1;
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
        let lines = init_lines(Some(&on()), "esp32c3");
        assert!(
            lines.contains("let mut flash = esp_storage::FlashStorage::new(peripherals.FLASH);")
        );
        assert!(lines.contains("use crate::pins::configs::flash_store;"));
        assert!(TAIL_SEED.contains("flash_store::ConfigStore::new(flash)"));
        assert!(init_lines(None, "esp32c3").is_empty());
    }
}
