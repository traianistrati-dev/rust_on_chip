//! Built-in MCU definitions, bundled into the binary via `include_str!`.
//!
//! These RON files (`assets/mcus/*.ron`) are the single source of truth for the
//! shipped chips.  Regenerate them from the factories with:
//!   `cargo test generate_builtin_ron -- --ignored`
//!
//! Later phases add a runtime folder scan for user-imported definitions.

use super::mcu_def::McuDefinition;

const STM32F103C8T6_RON: &str = include_str!("../../../assets/mcus/stm32f103c8t6.ron");
const ESP32C3_RON: &str = include_str!("../../../assets/mcus/esp32c3.ron");
// Generated from Espressif's own metadata — see `esp_gen::tests::regenerate_esp_ron`.
// The C3 above is NOT among them: it is hand-written from the datasheet and
// carries the real QFN32 pinout, which the metadata does not describe.
const ESP32_RON: &str = include_str!("../../../assets/mcus/esp32.ron");
const ESP32C2_RON: &str = include_str!("../../../assets/mcus/esp32c2.ron");
const ESP32C5_RON: &str = include_str!("../../../assets/mcus/esp32c5.ron");
const ESP32C6_RON: &str = include_str!("../../../assets/mcus/esp32c6.ron");
const ESP32C61_RON: &str = include_str!("../../../assets/mcus/esp32c61.ron");
const ESP32H2_RON: &str = include_str!("../../../assets/mcus/esp32h2.ron");
// Xtensa. They build under Espressif's fork of rustc, selected by the
// `rust-toolchain.toml` the generated project carries — see
// `project_gen::rust_toolchain_for`.
const ESP32S2_RON: &str = include_str!("../../../assets/mcus/esp32s2.ron");
const ESP32S3_RON: &str = include_str!("../../../assets/mcus/esp32s3.ron");

// Raspberry Pi. Boards, not bare chips: the pin numbers are the 40-pin header's,
// because that is what is silkscreened and what a user counts to. GP23/24/25/29
// are not on the header but are on the board — GP25 drives the LED, which is the
// first thing anyone reaches for.
const RP2040_PICO_RON: &str = include_str!("../../../assets/mcus/rp2040_pico.ron");
const RP2350_PICO2_RON: &str = include_str!("../../../assets/mcus/rp2350_pico2.ron");
// The wireless boards. Same silicon, same header — but GP23/24/25/29 belong to
// the CYW43 radio, and the LED with them. Everything else generates exactly as
// on the non-W board.
const RP2040_PICO_W_RON: &str = include_str!("../../../assets/mcus/rp2040_pico_w.ron");
const RP2350_PICO2_W_RON: &str = include_str!("../../../assets/mcus/rp2350_pico2_w.ron");
// tinyVision's pico2-ice: an RP2350B beside an iCE40UP5K FPGA, on two 2x20
// headers rather than the Pico's one. Generated from the board's netlist by
// `codegen::rp::pico2_ice_board`, which also checks this file against it.
const RP2350_PICO2_ICE_RON: &str = include_str!("../../../assets/mcus/rp2350_pico2_ice.ron");

// Nordic. A board again: the pads are the micro:bit's edge connector, and the
// nets wired to the LED matrix, buttons, speaker, microphone and sensors sit on
// `top`. Each name leads with the nRF port and pin the codegen reads.
const NRF52833_MICROBIT_V2_RON: &str =
    include_str!("../../../assets/mcus/nrf52833_microbit_v2.ron");
// Nordic's own kits. Generated from their pin tables by
// `codegen::nrf_boards`, which also checks these files against them.
const NRF52840_DK_RON: &str = include_str!("../../../assets/mcus/nrf52840_dk.ron");
const NRF52832_DK_RON: &str = include_str!("../../../assets/mcus/nrf52832_dk.ron");
const NRF5340_DK_RON: &str = include_str!("../../../assets/mcus/nrf5340_dk.ron");

/// Raw `(id, ron-text)` for every bundled chip.
const BUILTINS: &[(&str, &str)] = &[
    ("stm32f103c8t6", STM32F103C8T6_RON),
    ("esp32", ESP32_RON),
    ("esp32c2", ESP32C2_RON),
    ("esp32c3", ESP32C3_RON),
    ("esp32c5", ESP32C5_RON),
    ("esp32c6", ESP32C6_RON),
    ("esp32c61", ESP32C61_RON),
    ("esp32h2", ESP32H2_RON),
    ("esp32s2", ESP32S2_RON),
    ("esp32s3", ESP32S3_RON),
    ("rp2040_pico", RP2040_PICO_RON),
    ("rp2350_pico2", RP2350_PICO2_RON),
    ("rp2040_pico_w", RP2040_PICO_W_RON),
    ("rp2350_pico2_w", RP2350_PICO2_W_RON),
    ("rp2350_pico2_ice", RP2350_PICO2_ICE_RON),
    ("nrf52833_microbit_v2", NRF52833_MICROBIT_V2_RON),
    ("nrf52840_dk", NRF52840_DK_RON),
    ("nrf52832_dk", NRF52832_DK_RON),
    ("nrf5340_dk", NRF5340_DK_RON),
];

/// Parse all bundled built-in MCU definitions (bad files are skipped + logged).
///
/// The app calls this once, at startup. The tests call it about a hundred
/// times, and every call used to deserialise all 1.78 MB of RON again — so
/// under `cfg(test)` the parse runs once per test binary and each caller gets
/// its own clone. The running app keeps no second, cached copy.
pub fn builtin_definitions() -> Vec<McuDefinition> {
    if cfg!(test) {
        parsed_once().to_vec()
    } else {
        parse_all()
    }
}

/// [`parse_all`], run at most once per process. Test-only in effect: see
/// [`builtin_definitions`].
fn parsed_once() -> &'static [McuDefinition] {
    static PARSED: std::sync::OnceLock<Vec<McuDefinition>> = std::sync::OnceLock::new();
    PARSED.get_or_init(parse_all)
}

fn parse_all() -> Vec<McuDefinition> {
    BUILTINS
        .iter()
        .filter_map(|(id, ron)| match ron::from_str::<McuDefinition>(ron) {
            Ok(d) => Some(d),
            Err(e) => {
                // ASCII, not `⚠`: this goes to stderr, not to an egui label, and
                // a Windows console on a non-UTF-8 code page prints the symbol
                // as mojibake. The project's rule for log text is `[!]` / `[X]`.
                eprintln!("[!] failed to parse built-in MCU '{id}': {e}");
                None
            }
        })
        .collect()
}

/// Find a built-in definition by its `id` (e.g. "stm32f103c8t6").
pub fn builtin_for(id: &str) -> Option<McuDefinition> {
    if cfg!(test) {
        // Same cache as `builtin_definitions`. Matching the parsed `def.id` is
        // the table id: `every_table_id_is_its_definitions_id` pins that.
        return parsed_once().iter().find(|d| d.id == id).cloned();
    }
    BUILTINS
        .iter()
        .find(|(bid, _)| *bid == id)
        .and_then(|(_, ron)| ron::from_str::<McuDefinition>(ron).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `builtin_for` looks a chip up by the table id in the app, and by the
    /// parsed definition's id through the test cache. Both must name the same
    /// chip, and every bundled file must still parse.
    #[test]
    fn every_table_id_is_its_definitions_id() {
        for (id, ron) in BUILTINS {
            let def = ron::from_str::<McuDefinition>(ron)
                .unwrap_or_else(|e| panic!("built-in {id} does not parse: {e}"));
            assert_eq!(def.id, *id);
        }
        assert_eq!(builtin_definitions().len(), BUILTINS.len());
    }
    use crate::panels::mcu_module::clock::ClockConfig;
    use crate::panels::mcu_module::mcu::Mcu;
    use crate::panels::mcu_module::mcu_def::{
        ClockDef, McuDefinition, PinDef, PinLayout, ProjectDef,
    };
    use crate::panels::mcu_module::mock_esp32c3::create_esp32c3;
    use crate::panels::mcu_module::mock_mcu::create_stm32f103c8tx;

    fn layout(m: &Mcu) -> PinLayout {
        PinLayout {
            // Built-ins are all edge-packaged; a ball grid comes from a `.ron`.
            grid: None,
            top: m.top_pins.iter().map(PinDef::from_pin).collect(),
            bottom: m.bottom_pins.iter().map(PinDef::from_pin).collect(),
            left: m.left_pins.iter().map(PinDef::from_pin).collect(),
            right: m.right_pins.iter().map(PinDef::from_pin).collect(),
        }
    }

    fn def_of(
        id: &str,
        family: &str,
        package: &str,
        cpu: &str,
        max_mhz: u32,
        // On-die RAM, for the catalogue. `None` where `ProjectDef::ram_size`
        // already states it — the STM32s write their own `memory.x`, so the
        // linker figure IS the answer there.
        sram_kb: Option<u32>,
        m: &Mcu,
        project: ProjectDef,
    ) -> McuDefinition {
        let clock = match &m.clock {
            ClockConfig::Graph(g) => ClockDef::Graph(g.clone()),
            ClockConfig::None => ClockDef::None,
        };
        McuDefinition {
            board_chip: None,
            id: id.into(),
            display_name: m.name.clone(),
            family: family.into(),
            package: package.into(),
            sram_kb,
            max_mhz: Some(max_mhz),
            // Built-in chips keep using the hand-written family tables in
            // `codegen::dma_map`; only imported ones carry vendor DMA data.
            dma: None,
            irq_vectors: Vec::new(),
            usart_ip: None,
            sdmmc_ip: None,
            cpu: cpu.into(),
            toolchain: m.toolchain.clone(),
            project,
            pins: layout(m),
            clock,
            clock_limits: m.clock_limits,
            clock_presets: Vec::new(),
        }
    }

    /// One-shot: regenerate `assets/mcus/*.ron` from the factories.
    /// Run with:  `cargo test regenerate_builtin_ron -- --ignored`
    /// Project params are authored here (the single source for them).
    ///
    /// # LOSSY — check the diff before committing what this writes
    ///
    /// The factories carry pins and project settings; they do NOT carry the
    /// clock, and running this **overwrites the committed clock with whatever
    /// the bare factory has**:
    ///
    /// * `esp32c3.ron` loses `clock: Esp32c3` and becomes `clock: None` — the
    ///   ESP32-C3 silently ends up with no clock tree at all.
    /// * `stm32f103c8t6.ron` has its compact, authored `clock: Stm32f1(…)`
    ///   expanded into a thousand-line `Graph(…)`, because `build_mcu` upgrades
    ///   the compact form on load. That is not merely verbose: presets are
    ///   filtered by clock family, so `Stm32f1` presets stop applying to it.
    ///
    /// `esp32c3_ron_matches_factory` does not catch either — it deliberately
    /// compares pins and toolchain only, for exactly this reason.
    ///
    /// So: run it, `git diff`, and keep only the hunks you meant.
    #[test]
    #[ignore]
    fn regenerate_builtin_ron() {
        use ron::ser::PrettyConfig;
        std::fs::create_dir_all("assets/mcus").unwrap();
        let pretty = PrettyConfig::default().struct_names(true);

        let stm = def_of(
            "stm32f103c8t6", "stm32f1", "LQFP48", "ARM Cortex-M3", 72,
            None, // stated by `ram_size` below
            &create_stm32f103c8tx(),
            ProjectDef {
                pkg_name: "stm32f103c8t6".into(),
                target: "thumbv7m-none-eabi".into(),
                flash_origin: "0x08000000".into(),
                flash_size: "64K".into(),
                ram_origin: "0x20000000".into(),
                ram_size: "20K".into(),
                hal_dep: r#"stm32f1xx-hal = { version = "0.10", features = ["stm32f103", "medium", "rt"] }"#.into(),
                hal_dep_async: None,
                probe_chip: "STM32F103C8".into(),
                memory_comment: "STM32F103C8T6  —  64 KiB Flash / 20 KiB RAM".into(),
            },
        );
        let esp = def_of(
            "esp32c3",
            "esp32c3",
            "QFN32",
            "RISC-V 32-bit",
            160,
            // 393216 bytes of DRAM, per Espressif's own metadata. Its FLASH
            // stays unknown: that is an external SPI part chosen by the module.
            Some(384),
            &create_esp32c3(),
            ProjectDef {
                pkg_name: "esp32c3".into(),
                target: "riscv32imc-unknown-none-elf".into(),
                flash_origin: String::new(),
                flash_size: String::new(),
                ram_origin: String::new(),
                ram_size: String::new(),
                hal_dep: r#"esp-hal = { version = "0.23", features = ["esp32c3", "unstable"] }"#
                    .into(),
                hal_dep_async: None,
                probe_chip: "esp32c3".into(),
                memory_comment: String::new(),
            },
        );

        for def in [&stm, &esp] {
            let ron = ron::ser::to_string_pretty(def, pretty.clone()).unwrap();
            let ron = crate::panels::mcu_module::ron_text::bare_none(&ron);
            std::fs::write(format!("assets/mcus/{}.ron", def.id), ron).unwrap();
        }
    }

    /// One-shot: (re)generate the **example** importable definition shipped in
    /// `assets/mcus/examples/stm32f103rb.ron` — a real STM32F103RBT6 in the
    /// LQFP64 package (64 pins). This is *not* a built-in; it demonstrates the
    /// runtime "Import MCU…" flow for a new chip inside the supported STM32F1
    /// family. Run with: `cargo test generate_stm32f103rb_example -- --ignored`
    #[test]
    #[ignore]
    fn generate_stm32f103rb_example() {
        use crate::panels::mcu_module::mcu_catalog::ToolchainKind;
        use crate::panels::mcu_module::pins::logic::pin::Pin;
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction as F;
        use ron::ser::PrettyConfig;

        // STM32F103RBT6 LQFP64 pinout (datasheet DS5319). Functions mirror the
        // STM32F103xB die used by the built-in C8T6, plus the pins LQFP64 adds
        // (full PORTC + PD0..PD2).

        // ── LEFT — pins 1..16 (top→bottom) ──────────────────────────
        let left = vec![
            Pin::new_reserved(1, "VBAT"),
            Pin::new(2, "PC13"),
            Pin::new(3, "PC14"),
            Pin::new(4, "PC15"),
            Pin::new(5, "PD0"),
            Pin::new(6, "PD1"),
            Pin::new_reserved(7, "NRST"),
            Pin::new_with_analog(8, "PC0", 1, 10),
            Pin::new_with_analog(9, "PC1", 1, 11),
            Pin::new_with_analog(10, "PC2", 1, 12),
            Pin::new_with_analog(11, "PC3", 1, 13),
            Pin::new_reserved(12, "VSSA"),
            Pin::new_reserved(13, "VDDA"),
            Pin::new_with_analog(14, "PA0", 1, 0).with_functions(vec![
                F::UsartCts(2),
                F::TimerPwm {
                    timer: 2,
                    channel: 1,
                },
            ]),
            Pin::new_with_analog(15, "PA1", 1, 1).with_functions(vec![
                F::UsartRts(2),
                F::TimerPwm {
                    timer: 2,
                    channel: 2,
                },
            ]),
            Pin::new_with_analog(16, "PA2", 1, 2).with_functions(vec![
                F::UsartTx(2),
                F::TimerPwm {
                    timer: 2,
                    channel: 3,
                },
            ]),
        ];

        // ── BOTTOM — pins 17..32 (left→right) ───────────────────────
        let bottom = vec![
            Pin::new_with_analog(17, "PA3", 1, 3).with_functions(vec![
                F::UsartRx(2),
                F::TimerPwm {
                    timer: 2,
                    channel: 4,
                },
            ]),
            Pin::new_reserved(18, "VSS"),
            Pin::new_reserved(19, "VDD"),
            Pin::new_with_analog(20, "PA4", 1, 4).with_functions(vec![F::SpiNss(1), F::UsartCk(2)]),
            Pin::new_with_analog(21, "PA5", 1, 5).with_functions(vec![F::SpiSck(1)]),
            Pin::new_with_analog(22, "PA6", 1, 6).with_functions(vec![
                F::SpiMiso(1),
                F::TimerPwm {
                    timer: 3,
                    channel: 1,
                },
            ]),
            Pin::new_with_analog(23, "PA7", 1, 7).with_functions(vec![
                F::SpiMosi(1),
                F::TimerPwm {
                    timer: 3,
                    channel: 2,
                },
            ]),
            Pin::new_with_analog(24, "PC4", 1, 14),
            Pin::new_with_analog(25, "PC5", 1, 15),
            Pin::new_with_analog(26, "PB0", 1, 8).with_functions(vec![F::TimerPwm {
                timer: 3,
                channel: 3,
            }]),
            Pin::new_with_analog(27, "PB1", 1, 9).with_functions(vec![F::TimerPwm {
                timer: 3,
                channel: 4,
            }]),
            Pin::new(28, "PB2"),
            Pin::new(29, "PB10").with_functions(vec![F::I2cScl(2), F::UsartTx(3)]),
            Pin::new(30, "PB11").with_functions(vec![F::I2cSda(2), F::UsartRx(3)]),
            Pin::new_reserved(31, "VSS"),
            Pin::new_reserved(32, "VDD"),
        ];

        // ── RIGHT — pins 48..33 (top→bottom) ────────────────────────
        let right = vec![
            Pin::new_reserved(48, "VDD"),
            Pin::new_reserved(47, "VSS"),
            Pin::new(46, "PA13").with_functions(vec![F::SwdIo]),
            Pin::new(45, "PA12").with_functions(vec![F::UsbDp, F::CanTx, F::UsartRts(1)]),
            Pin::new(44, "PA11").with_functions(vec![
                F::UsbDm,
                F::CanRx,
                F::UsartCts(1),
                F::TimerPwm {
                    timer: 1,
                    channel: 4,
                },
            ]),
            Pin::new(43, "PA10").with_functions(vec![
                F::UsartRx(1),
                F::TimerPwm {
                    timer: 1,
                    channel: 3,
                },
            ]),
            Pin::new(42, "PA9").with_functions(vec![
                F::UsartTx(1),
                F::TimerPwm {
                    timer: 1,
                    channel: 2,
                },
            ]),
            Pin::new(41, "PA8").with_functions(vec![
                F::Mco,
                F::UsartCk(1),
                F::TimerPwm {
                    timer: 1,
                    channel: 1,
                },
            ]),
            Pin::new(40, "PC9").with_functions(vec![F::TimerPwm {
                timer: 3,
                channel: 4,
            }]),
            Pin::new(39, "PC8").with_functions(vec![F::TimerPwm {
                timer: 3,
                channel: 3,
            }]),
            Pin::new(38, "PC7").with_functions(vec![F::TimerPwm {
                timer: 3,
                channel: 2,
            }]),
            Pin::new(37, "PC6").with_functions(vec![F::TimerPwm {
                timer: 3,
                channel: 1,
            }]),
            Pin::new(36, "PB15").with_functions(vec![F::SpiMosi(2)]),
            Pin::new(35, "PB14").with_functions(vec![F::SpiMiso(2), F::UsartRts(3)]),
            Pin::new(34, "PB13").with_functions(vec![F::SpiSck(2), F::UsartCts(3)]),
            Pin::new(33, "PB12").with_functions(vec![F::SpiNss(2), F::UsartCk(3), F::I2cScl(2)]),
        ];

        // ── TOP — pins 64..49 (left→right) ──────────────────────────
        let top = vec![
            Pin::new_reserved(64, "VDD"),
            Pin::new_reserved(63, "VSS"),
            Pin::new(62, "PB9").with_functions(vec![
                F::TimerPwm {
                    timer: 4,
                    channel: 4,
                },
                F::CanTx,
                F::I2cSda(1),
            ]),
            Pin::new(61, "PB8").with_functions(vec![
                F::TimerPwm {
                    timer: 4,
                    channel: 3,
                },
                F::CanRx,
                F::I2cScl(1),
            ]),
            Pin::new_reserved(60, "BOOT0"),
            Pin::new(59, "PB7").with_functions(vec![
                F::I2cSda(1),
                F::TimerPwm {
                    timer: 4,
                    channel: 2,
                },
            ]),
            Pin::new(58, "PB6").with_functions(vec![
                F::I2cScl(1),
                F::TimerPwm {
                    timer: 4,
                    channel: 1,
                },
            ]),
            Pin::new(57, "PB5").with_functions(vec![
                F::SpiMosi(1),
                F::TimerPwm {
                    timer: 3,
                    channel: 2,
                },
            ]),
            Pin::new(56, "PB4").with_functions(vec![
                F::SpiMiso(1),
                F::TimerPwm {
                    timer: 3,
                    channel: 1,
                },
            ]),
            Pin::new(55, "PB3").with_functions(vec![
                F::SpiSck(1),
                F::TimerPwm {
                    timer: 2,
                    channel: 2,
                },
            ]),
            Pin::new(54, "PD2"),
            Pin::new(53, "PC12").with_functions(vec![F::UsartCk(3)]),
            Pin::new(52, "PC11").with_functions(vec![F::UsartRx(3)]),
            Pin::new(51, "PC10").with_functions(vec![F::UsartTx(3)]),
            Pin::new(50, "PA15").with_functions(vec![
                F::SpiNss(1),
                F::TimerPwm {
                    timer: 2,
                    channel: 1,
                },
            ]),
            Pin::new(49, "PA14").with_functions(vec![F::SwdClk]),
        ];

        let m = Mcu::new(
            "STM32F103RBT6".to_owned(),
            "stm32f1".to_owned(),
            ToolchainKind::RustEmbedded,
            top,
            bottom,
            left,
            right,
        );

        let mut def = def_of(
            "stm32f103rb", "stm32f1", "LQFP64", "ARM Cortex-M3", 72,
            None,
            &m,
            ProjectDef {
                pkg_name: "stm32f103rb".into(),
                target: "thumbv7m-none-eabi".into(),
                flash_origin: "0x08000000".into(),
                flash_size: "128K".into(),
                ram_origin: "0x20000000".into(),
                ram_size: "20K".into(),
                hal_dep: r#"stm32f1xx-hal = { version = "0.10", features = ["stm32f103", "medium", "rt"] }"#.into(),
                hal_dep_async: None,
                probe_chip: "STM32F103RB".into(),
                memory_comment: "STM32F103RBT6  —  128 KiB Flash / 20 KiB RAM (LQFP64)".into(),
            },
        );

        // Demonstrate a chip-specific preset in the importable format (when
        // `clock_presets` is non-empty it replaces the family defaults in the
        // Clock tab). 48 MHz keeps USB valid with the /1 prescaler.
        {
            use crate::panels::mcu_module::clock::model::{Stm32f1Clock, UsbPre};
            use crate::panels::mcu_module::mcu_def::ClockPresetDef;
            def.clock_presets = vec![
                ClockPresetDef {
                    name: "72 MHz (HSE 8 + PLL×9)".into(),
                    description: "Max performance. SYSCLK 72, PCLK1 36, USB 48 MHz.".into(),
                    config: ClockDef::Stm32f1(Stm32f1Clock::default()),
                },
                ClockPresetDef {
                    name: "48 MHz USB (HSE 8 + PLL×6)".into(),
                    description: "SYSCLK 48, PCLK1 24, USBCLK 48 via /1 prescaler.".into(),
                    config: ClockDef::Stm32f1(Stm32f1Clock {
                        pll_mul: 6,
                        adc_pre: 4, // 48/1/4 = 12 ≤ 14
                        usb_pre: UsbPre::Div1,
                        ..Stm32f1Clock::default()
                    }),
                },
            ];
        }

        std::fs::create_dir_all("assets/mcus/examples").unwrap();
        let ron =
            ron::ser::to_string_pretty(&def, PrettyConfig::default().struct_names(true)).unwrap();
        let ron = crate::panels::mcu_module::ron_text::bare_none(&ron);
        std::fs::write("assets/mcus/examples/stm32f103rb.ron", ron).unwrap();
    }

    /// The shipped example file must stay parseable and build into a 64-pin Mcu.
    /// Read at runtime (not `include_str!`) so the suite compiles even before the
    /// generator has produced the file.
    #[test]
    fn stm32f103rb_example_is_valid() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/mcus/examples/stm32f103rb.ron"
        );
        let text = std::fs::read_to_string(path).expect("example file exists (run generator)");
        let def: McuDefinition = ron::from_str(&text).expect("example RON parses");
        assert_eq!(def.id, "stm32f103rb");
        assert_eq!(def.family, "stm32f1");
        let mcu = def.build_mcu();
        let total =
            mcu.top_pins.len() + mcu.bottom_pins.len() + mcu.left_pins.len() + mcu.right_pins.len();
        assert_eq!(total, 64, "LQFP64 must expose exactly 64 pins");
        // A family backend exists, so it generates real code.
        assert!(
            !mcu.fresh_main_rs().is_empty(),
            "stm32f1 example must generate code"
        );
    }

    /// One-shot: write an example chip that uses a **data-driven `ClockDef::Graph`**
    /// (the F103 tree + diagram as data) so the generic graph clock tab can be
    /// exercised by importing it. Run with:
    /// `cargo test generate_graph_clock_example -- --ignored`
    #[test]
    #[ignore]
    fn generate_graph_clock_example() {
        use crate::panels::mcu_module::clock::graph::layout::stm32f1_layout;
        use crate::panels::mcu_module::clock::graph::{GraphClock, stm32f1_graph};
        use crate::panels::mcu_module::clock::model::{ClockLimits, Stm32f1Clock};
        use ron::ser::PrettyConfig;

        // Start from the built-in C8T6, swap the clock for an embedded graph.
        let mut def = builtin_for("stm32f103c8t6").expect("base def");
        def.id = "stm32f103c8t6-graph".to_owned();
        def.display_name = "STM32F103C8T6".to_owned();
        def.clock = ClockDef::Graph(GraphClock {
            graph: stm32f1_graph(&Stm32f1Clock::default()),
            layout: stm32f1_layout(&ClockLimits::default()),
            bindings: Default::default(),
        });

        std::fs::create_dir_all("assets/mcus/examples").unwrap();
        let ron =
            ron::ser::to_string_pretty(&def, PrettyConfig::default().struct_names(true)).unwrap();
        let ron = crate::panels::mcu_module::ron_text::bare_none(&ron);
        std::fs::write("assets/mcus/examples/stm32f103_graphclock.ron", ron).unwrap();
    }

    /// The graph-clock example parses and builds into a `ClockConfig::Graph`
    /// carrying both the evaluatable graph and its diagram layout.
    #[test]
    fn graph_clock_example_is_valid() {
        use crate::panels::mcu_module::clock::ClockConfig;

        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/mcus/examples/stm32f103_graphclock.ron"
        );
        let text = std::fs::read_to_string(path).expect("example exists (run generator)");
        let def: McuDefinition = ron::from_str(&text).expect("graph-clock example parses");
        assert_eq!(def.id, "stm32f103c8t6-graph");
        assert!(matches!(def.clock, ClockDef::Graph(_)));

        match def.build_mcu().clock {
            ClockConfig::Graph(gc) => {
                assert!(!gc.graph.nodes.is_empty(), "graph must carry nodes");
                assert!(
                    !gc.layout.outputs.is_empty(),
                    "layout must carry the diagram"
                );
            }
            _ => panic!("expected a graph clock"),
        }
    }

    /// One-shot: write an **ESP32-C3** chip whose clock is a data-driven
    /// `ClockDef::Graph` (a totally different tree from STM32F1), proving the
    /// graph clock is family-agnostic. Run with:
    /// `cargo test generate_esp32c3_graph_clock_example -- --ignored`
    #[test]
    #[ignore]
    fn generate_esp32c3_graph_clock_example() {
        use crate::panels::mcu_module::clock::graph::{GraphClock, esp32c3_graph, esp32c3_layout};
        use ron::ser::PrettyConfig;

        let mut def = builtin_for("esp32c3").expect("base def");
        def.id = "esp32c3-graph".to_owned();
        def.display_name = "ESP32-C3".to_owned();
        def.clock = ClockDef::Graph(GraphClock {
            graph: esp32c3_graph(),
            layout: esp32c3_layout(),
            bindings: Default::default(),
        });

        std::fs::create_dir_all("assets/mcus/examples").unwrap();
        let ron =
            ron::ser::to_string_pretty(&def, PrettyConfig::default().struct_names(true)).unwrap();
        let ron = crate::panels::mcu_module::ron_text::bare_none(&ron);
        std::fs::write("assets/mcus/examples/esp32c3_graphclock.ron", ron).unwrap();
    }

    /// The ESP32-C3 graph-clock example parses, builds, and evaluates to the
    /// expected default frequencies (CPU 160 MHz).
    #[test]
    fn esp32c3_graph_clock_example_is_valid() {
        use crate::panels::mcu_module::clock::ClockConfig;
        use crate::panels::mcu_module::clock::graph::evaluate;

        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/mcus/examples/esp32c3_graphclock.ron"
        );
        let text = std::fs::read_to_string(path).expect("example exists (run generator)");
        let def: McuDefinition = ron::from_str(&text).expect("esp32c3 graph-clock parses");
        assert_eq!(def.id, "esp32c3-graph");
        assert!(matches!(def.clock, ClockDef::Graph(_)));

        match def.build_mcu().clock {
            ClockConfig::Graph(gc) => {
                let f = evaluate(&gc.graph);
                assert_eq!(
                    f.get("cpu").copied().unwrap_or(0),
                    160_000_000,
                    "CPU 160 MHz"
                );
                assert_eq!(f.get("apb").copied().unwrap_or(0), 80_000_000, "APB 80 MHz");
                assert!(
                    !gc.layout.outputs.is_empty(),
                    "layout must carry the diagram"
                );
            }
            _ => panic!("expected a graph clock"),
        }
    }

    #[test]
    fn all_builtins_parse() {
        let defs = builtin_definitions();
        assert_eq!(defs.len(), BUILTINS.len(), "every bundled RON must parse");
    }

    #[test]
    fn builtins_build_into_mcus() {
        for (id, _) in BUILTINS {
            let def = builtin_for(id).unwrap_or_else(|| panic!("missing {id}"));
            let mcu = def.build_mcu();
            // A real chip has at least one configurable pin somewhere.
            let total = mcu.top_pins.len()
                + mcu.bottom_pins.len()
                + mcu.left_pins.len()
                + mcu.right_pins.len();
            assert!(total > 0, "{id} produced no pins");
        }
    }

    /// The bundled RON must build chips identical to the original factories.
    #[test]
    fn esp32c3_ron_matches_factory() {
        use crate::panels::mcu_module::mock_esp32c3::create_esp32c3;
        use crate::panels::mcu_module::pins::logic::pin::Pin;

        let factory = create_esp32c3();
        let built = builtin_for("esp32c3").unwrap().build_mcu();
        let same = |a: &[Pin], b: &[Pin]| {
            a.len() == b.len()
                && a.iter().zip(b).all(|(x, y)| {
                    x.number == y.number
                        && x.name == y.name
                        && x.reserved == y.reserved
                        && x.available_functions == y.available_functions
                })
        };
        assert!(same(&built.top_pins, &factory.top_pins));
        assert!(same(&built.bottom_pins, &factory.bottom_pins));
        assert!(same(&built.left_pins, &factory.left_pins));
        assert!(same(&built.right_pins, &factory.right_pins));
        assert_eq!(built.toolchain, factory.toolchain);
        // The built-in now ships a graph clock (`ClockDef::Esp32c3`); the bare
        // factory has none, so only the pin/toolchain identity is compared.
        use crate::panels::mcu_module::clock::ClockConfig;
        assert!(
            matches!(built.clock, ClockConfig::Graph(_)),
            "esp32c3 built-in carries a graph clock"
        );
    }
}
