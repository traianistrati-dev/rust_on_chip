use super::mcu::Mcu;
use super::mcu_catalog::ToolchainKind;
use super::pins::logic::pin::Pin;
use super::pins::logic::pin_function::PinFunction;

/// Builds the STM32F103C8Tx MCU with all 48 pins correctly mapped.
/// Peripheral functions sourced from the official STM32F103C8T6 datasheet (DS5319).
pub fn create_stm32f103c8tx() -> Mcu {
    // ── TOP pins — drawn left→right, chip pins 48 down to 37 ────────────────
    let top_pins = vec![
        // Pin 48: VDD
        Pin::new_reserved(48, "VDD"),
        // Pin 47: VSS
        Pin::new_reserved(47, "VSS"),
        // Pin 46: PB9 — TIM4_CH4, CAN_TX (remap)
        Pin::new(46, "PB9").with_functions(vec![
            PinFunction::TimerPwm {
                timer: 4,
                channel: 4,
            },
            PinFunction::CanTx,
            PinFunction::I2cSda(1), // I2C1_SDA remap
        ]),
        // Pin 45: PB8 — TIM4_CH3, CAN_RX (remap), I2C1_SCL (remap)
        Pin::new(45, "PB8").with_functions(vec![
            PinFunction::TimerPwm {
                timer: 4,
                channel: 3,
            },
            PinFunction::CanRx,
            PinFunction::I2cScl(1), // I2C1_SCL remap
        ]),
        // Pin 44: BOOT0 — reserved
        Pin::new_reserved(44, "BOOT0"),
        // Pin 43: PB7 — I2C1_SDA, TIM4_CH2
        Pin::new(43, "PB7").with_functions(vec![
            PinFunction::I2cSda(1),
            PinFunction::TimerPwm {
                timer: 4,
                channel: 2,
            },
        ]),
        // Pin 42: PB6 — I2C1_SCL, TIM4_CH1
        Pin::new(42, "PB6").with_functions(vec![
            PinFunction::I2cScl(1),
            PinFunction::TimerPwm {
                timer: 4,
                channel: 1,
            },
        ]),
        // Pin 41: PB5 — SPI1_MOSI (remap), TIM3_CH2 (remap), I2C1_SMBA
        Pin::new(41, "PB5").with_functions(vec![
            PinFunction::SpiMosi(1),
            PinFunction::TimerPwm {
                timer: 3,
                channel: 2,
            },
        ]),
        // Pin 40: PB4 — SPI1_MISO (remap), TIM3_CH1 (remap)  [default: JNTRST]
        Pin::new(40, "PB4").with_functions(vec![
            PinFunction::SpiMiso(1),
            PinFunction::TimerPwm {
                timer: 3,
                channel: 1,
            },
        ]),
        // Pin 39: PB3 — SPI1_SCK (remap), TIM2_CH2 (remap)  [default: JTDO]
        Pin::new(39, "PB3").with_functions(vec![
            PinFunction::SpiSck(1),
            PinFunction::TimerPwm {
                timer: 2,
                channel: 2,
            },
        ]),
        // Pin 38: PA15 — SPI1_NSS (remap), TIM2_CH1 (remap)  [default: JTDI]
        Pin::new(38, "PA15").with_functions(vec![
            PinFunction::SpiNss(1),
            PinFunction::TimerPwm {
                timer: 2,
                channel: 1,
            },
        ]),
        // Pin 37: PA14 — SWCLK (JTCK) — can be GPIO after remap
        Pin::new(37, "PA14").with_functions(vec![PinFunction::SwdClk]),
    ];

    // ── BOTTOM pins — drawn left→right, chip pins 13 to 24 ──────────────────
    let bottom_pins = vec![
        // Pin 13: PA3 — ADC12_IN3, USART2_RX, TIM2_CH4
        Pin::new_with_analog(13, "PA3", 1, 3).with_functions(vec![
            PinFunction::UsartRx(2),
            PinFunction::TimerPwm {
                timer: 2,
                channel: 4,
            },
        ]),
        // Pin 14: PA4 — ADC12_IN4, SPI1_NSS, USART2_CK
        Pin::new_with_analog(14, "PA4", 1, 4)
            .with_functions(vec![PinFunction::SpiNss(1), PinFunction::UsartCk(2)]),
        // Pin 15: PA5 — ADC12_IN5, SPI1_SCK
        Pin::new_with_analog(15, "PA5", 1, 5).with_functions(vec![PinFunction::SpiSck(1)]),
        // Pin 16: PA6 — ADC12_IN6, SPI1_MISO, TIM3_CH1
        Pin::new_with_analog(16, "PA6", 1, 6).with_functions(vec![
            PinFunction::SpiMiso(1),
            PinFunction::TimerPwm {
                timer: 3,
                channel: 1,
            },
        ]),
        // Pin 17: PA7 — ADC12_IN7, SPI1_MOSI, TIM3_CH2
        Pin::new_with_analog(17, "PA7", 1, 7).with_functions(vec![
            PinFunction::SpiMosi(1),
            PinFunction::TimerPwm {
                timer: 3,
                channel: 2,
            },
        ]),
        // Pin 18: PB0 — ADC12_IN8, TIM3_CH3
        Pin::new_with_analog(18, "PB0", 1, 8).with_functions(vec![PinFunction::TimerPwm {
            timer: 3,
            channel: 3,
        }]),
        // Pin 19: PB1 — ADC12_IN9, TIM3_CH4
        Pin::new_with_analog(19, "PB1", 1, 9).with_functions(vec![PinFunction::TimerPwm {
            timer: 3,
            channel: 4,
        }]),
        // Pin 20: PB2/BOOT1 — GPIO only
        Pin::new(20, "PB2"),
        // Pin 21: PB10 — I2C2_SCL, USART3_TX
        Pin::new(21, "PB10").with_functions(vec![PinFunction::I2cScl(2), PinFunction::UsartTx(3)]),
        // Pin 22: PB11 — I2C2_SDA, USART3_RX
        Pin::new(22, "PB11").with_functions(vec![PinFunction::I2cSda(2), PinFunction::UsartRx(3)]),
        // Pin 23: VSS
        Pin::new_reserved(23, "VSS"),
        // Pin 24: VDD
        Pin::new_reserved(24, "VDD"),
    ];

    // ── LEFT pins — drawn top→bottom, chip pins 1 to 12 ─────────────────────
    let left_pins = vec![
        // Pin 1: VBAT
        Pin::new_reserved(1, "VBAT"),
        // Pin 2: PC13 — GPIO only (TAMPER-RTC on some packages, not on C8T6)
        Pin::new(2, "PC13"),
        // Pin 3: PC14 — OSC32_IN / GPIO
        Pin::new(3, "PC14"),
        // Pin 4: PC15 — OSC32_OUT / GPIO
        Pin::new(4, "PC15"),
        // Pin 5: PD0 — OSC_IN / GPIO
        Pin::new(5, "PD0"),
        // Pin 6: PD1 — OSC_OUT / GPIO
        Pin::new(6, "PD1"),
        // Pin 7: NRST
        Pin::new_reserved(7, "NRST"),
        // Pin 8: VSSA
        Pin::new_reserved(8, "VSSA"),
        // Pin 9: VDDA
        Pin::new_reserved(9, "VDDA"),
        // Pin 10: PA0-WKUP — ADC12_IN0, USART2_CTS, TIM2_CH1/ETR
        Pin::new_with_analog(10, "PA0", 1, 0).with_functions(vec![
            PinFunction::UsartCts(2),
            PinFunction::TimerPwm {
                timer: 2,
                channel: 1,
            },
        ]),
        // Pin 11: PA1 — ADC12_IN1, USART2_RTS, TIM2_CH2
        Pin::new_with_analog(11, "PA1", 1, 1).with_functions(vec![
            PinFunction::UsartRts(2),
            PinFunction::TimerPwm {
                timer: 2,
                channel: 2,
            },
        ]),
        // Pin 12: PA2 — ADC12_IN2, USART2_TX, TIM2_CH3
        Pin::new_with_analog(12, "PA2", 1, 2).with_functions(vec![
            PinFunction::UsartTx(2),
            PinFunction::TimerPwm {
                timer: 2,
                channel: 3,
            },
        ]),
    ];

    // ── RIGHT pins — drawn top→bottom, chip pins 36 down to 25 ──────────────
    let right_pins = vec![
        // Pin 36: VDD
        Pin::new_reserved(36, "VDD"),
        // Pin 35: VSS
        Pin::new_reserved(35, "VSS"),
        // Pin 34: PA13 — SWDIO (JTMS) — can be GPIO after remap
        Pin::new(34, "PA13").with_functions(vec![PinFunction::SwdIo]),
        // Pin 33: PA12 — USB_DP, CAN_TX, USART1_RTS, TIM1_ETR
        Pin::new(33, "PA12").with_functions(vec![
            PinFunction::UsbDp,
            PinFunction::CanTx,
            PinFunction::UsartRts(1),
        ]),
        // Pin 32: PA11 — USB_DM, CAN_RX, USART1_CTS, TIM1_CH4
        Pin::new(32, "PA11").with_functions(vec![
            PinFunction::UsbDm,
            PinFunction::CanRx,
            PinFunction::UsartCts(1),
            PinFunction::TimerPwm {
                timer: 1,
                channel: 4,
            },
        ]),
        // Pin 31: PA10 — USART1_RX, TIM1_CH3
        Pin::new(31, "PA10").with_functions(vec![
            PinFunction::UsartRx(1),
            PinFunction::TimerPwm {
                timer: 1,
                channel: 3,
            },
        ]),
        // Pin 30: PA9 — USART1_TX, TIM1_CH2
        Pin::new(30, "PA9").with_functions(vec![
            PinFunction::UsartTx(1),
            PinFunction::TimerPwm {
                timer: 1,
                channel: 2,
            },
        ]),
        // Pin 29: PA8 — MCO, USART1_CK, TIM1_CH1
        Pin::new(29, "PA8").with_functions(vec![
            PinFunction::Mco,
            PinFunction::UsartCk(1),
            PinFunction::TimerPwm {
                timer: 1,
                channel: 1,
            },
        ]),
        // Pin 28: PB15 — SPI2_MOSI, TIM1_CH3N
        Pin::new(28, "PB15").with_functions(vec![PinFunction::SpiMosi(2)]),
        // Pin 27: PB14 — SPI2_MISO, USART3_RTS, TIM1_CH2N
        Pin::new(27, "PB14")
            .with_functions(vec![PinFunction::SpiMiso(2), PinFunction::UsartRts(3)]),
        // Pin 26: PB13 — SPI2_SCK, USART3_CTS, TIM1_CH1N
        Pin::new(26, "PB13").with_functions(vec![PinFunction::SpiSck(2), PinFunction::UsartCts(3)]),
        // Pin 25: PB12 — SPI2_NSS, USART3_CK, I2C2_SMBA, TIM1_BKIN. SMBA has
        // no PinFunction, and it is NOT I2C2's SCL (that is PB10): offered
        // as `I2cScl(2)` it generated an I2C2 on the wrong pin.
        Pin::new(25, "PB12").with_functions(vec![PinFunction::SpiNss(2), PinFunction::UsartCk(3)]),
    ];

    Mcu::new(
        "STM32F103C8Tx".to_owned(),
        "stm32f1".to_owned(),
        ToolchainKind::RustEmbedded,
        top_pins,
        bottom_pins,
        left_pins,
        right_pins,
    )
}

/// The STM32C011D6Yx in WLCSP12 — the tree's ball-grid chip, built from the
/// shipped example rather than hand-typed.
///
/// Its four side vecs come out EMPTY and all twelve pins are grid cells, which
/// is the property most tests about ball grids actually turn on: a package
/// whose pins are all inside the body. Reading the `.ron` keeps that property
/// tied to real shipped data instead of to a literal a refactor could quietly
/// change.
pub fn create_wlcsp12() -> Mcu {
    const SRC: &str = include_str!("../../../assets/mcus/examples/stm32c011d6yx_wlcsp12.ron");
    ron::from_str::<super::mcu_def::McuDefinition>(SRC)
        .expect("the shipped WLCSP12 example must parse")
        .build_mcu()
}

/// A genuinely 2-sided (DIP-style) chip: pins on the left and right only.
///
/// Hand-built because the tree ships none — every `.ron` in `assets/mcus` fills
/// three or four sides, and a two-row package only ever arrives from an import
/// (`stm32_pin_data::distribute_sides_2row`). Without it the 90° branch of the
/// rotation rule has nothing to test it with.
pub fn create_two_sided() -> Mcu {
    let left = (1..=4).map(|n| Pin::new(n, &format!("PA{n}"))).collect();
    let right = (5..=8).map(|n| Pin::new(n, &format!("PB{n}"))).collect();
    Mcu::new(
        "SO8N-ish".to_owned(),
        "stm32f0".to_owned(),
        ToolchainKind::RustEmbedded,
        vec![],
        vec![],
        left,
        right,
    )
}
