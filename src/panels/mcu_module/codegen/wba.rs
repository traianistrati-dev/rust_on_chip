//! STM32WBA family codegen — `embassy-stm32` in BLOCKING style.
//!
//! Why embassy and not a classic `stm32wbaxx-hal`: there is no mature blocking
//! HAL crate for the STM32WBA series, but `embassy-stm32` genuinely supports it
//! and its GPIO API (`Output::new` / `Input::new`) is small and stable. Used
//! WITHOUT the async executor — `embassy_stm32::init()` hands back the
//! peripheral singletons and the `Output`/`Input` wrappers work fine under
//! `cortex_m_rt::entry`, so the generated project fits the same base
//! `Cargo.toml` (cortex-m-rt + panic-halt + the `hal_dep` line) every other
//! `RustEmbedded` family uses.
//!
//! Scope of v1: GPIO input/output pins get real, compiling init; bus / analog
//! pins (USART/SPI/I2C/ADC/…) are bound to their raw `embassy_stm32`
//! peripheral singleton (`let pa9 = p.PA9;`) with the role in the comment —
//! ready to hand to `embassy_stm32::usart::Uart::new(...)` etc. Every line
//! round-trips through [`super::parse_main_rs`], so the Pins canvas restores on
//! reopen. Full peripheral-driver generation (like the STM32F1 config files)
//! is a later step.

use super::super::clock::model::ClockConfig;
use super::embassy_common;
use crate::panels::mcu_module::pins::logic::pin::Pin;

// The header + splice are the generic embassy shape (shared with every STM32
// family); re-exported so `family.rs` and the tests keep their `wba::…` paths.
pub use embassy_common::{invariant_header, splice_section};

/// The WBA generated section — the shared embassy shape with the WBA RCC clock
/// block spliced in. The RCC mapping is now the FAMILY-keyed
/// [`super::rcc::graph_clock_block`] (`"stm32wba"` → `ReadSpec::wba()` +
/// `RccDescriptor::wba()`), so this no longer sniffs the graph's shape.
pub fn make_generated_section(
    mcu_name: &str,
    pins: &[&Pin],
    clock: &ClockConfig,
    custom_inits: &str,
    manual: bool,
) -> String {
    embassy_common::make_generated_section(
        mcu_name,
        pins,
        &super::rcc::graph_clock_block("stm32wba", clock, manual),
        custom_inits,
    )
}

#[cfg(test)]
mod tests {
    use super::super::{USER_TAIL, parse_main_rs};
    use super::*;
    use crate::panels::mcu_module::pins::logic::pin::Pin;
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

    fn pin(name: &str, func: PinFunction) -> Pin {
        Pin {
            name: name.into(),
            number: 0,
            reserved: false,
            available_functions: vec![func.clone()],
            selected_function: func,
            custom_label: String::new(),
            irq: None,
            irq_priority: Default::default(),
            io_mode: None,
            af: Vec::new(),
            fn_owner: Vec::new(),
            note: String::new(),
        }
    }

    /// A GPIO output + input + a bus pin generate compiling embassy code, and
    /// every configured pin round-trips through the shared parser.
    #[test]
    fn generates_and_round_trips_pins() {
        let pb5 = pin("PB5", PinFunction::GpioOutput);
        let pc13 = pin("PC13", PinFunction::GpioInput);
        let pa9 = pin("PA9", PinFunction::UsartTx(1));
        let refs: Vec<&Pin> = vec![&pb5, &pc13, &pa9];
        let section = make_generated_section("STM32WBA55CG", &refs, &ClockConfig::None, "", false);

        // Shape: gpio imports (both kinds), embassy init, one line per pin.
        assert!(section.contains("use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};"));
        assert!(section.contains("embassy_stm32::init(Default::default())"));
        assert!(section.contains(
            "let mut pb5_out = Output::new(p.PB5, Level::Low, Speed::Low); // GPIO Output"
        ));
        assert!(section.contains("let pc13_in = Input::new(p.PC13, Pull::None); // GPIO Input"));
        assert!(section.contains("let pa9_usart1_tx = p.PA9; // USART1  TX"));

        // Round-trip: the full file's GEN block parses back to the same pins,
        // INCLUDING the `let mut` output (the parser tweak this relies on).
        let file = format!(
            "{}{section}\n{USER_TAIL}",
            invariant_header("STM32WBA55CG", "stm32wba55cg")
        );
        let parsed = parse_main_rs(&file);
        assert!(parsed.contains(&("PB5".into(), PinFunction::GpioOutput)));
        assert!(parsed.contains(&("PC13".into(), PinFunction::GpioInput)));
        assert!(parsed.contains(&("PA9".into(), PinFunction::UsartTx(1))));
    }

    /// embassy takes the pull as an ARGUMENT, so the same per-pin mode choice
    /// that picks `into_pull_up_input` on the blocking HAL comes out as
    /// `Pull::Up` here — the point of keeping the mode a neutral key.
    #[test]
    fn gpio_mode_maps_to_the_embassy_pull_argument() {
        use crate::panels::mcu_module::pins::logic::pin::GpioMode;
        let mut pc13 = pin("PC13", PinFunction::GpioInput);
        pc13.io_mode = Some(GpioMode::PullUp);
        let refs: Vec<&Pin> = vec![&pc13];
        let section = make_generated_section("STM32WBA55CG", &refs, &ClockConfig::None, "", false);
        assert!(
            section.contains("let pc13_in = Input::new(p.PC13, Pull::Up); // GPIO Input"),
            "{section}"
        );
    }

    /// No configured pins → no gpio import, a placeholder comment, still valid.
    #[test]
    fn empty_config_omits_imports() {
        let section = make_generated_section("STM32WBA55CG", &[], &ClockConfig::None, "", false);
        assert!(!section.contains("use embassy_stm32::gpio"));
        assert!(section.contains("No pins configured yet"));
        assert!(section.contains("fn main() -> !"));
        // No WBA clock graph → embassy's own default init.
        assert!(section.contains("embassy_stm32::init(Default::default())"));
    }

    /// The Clock-tab graph maps onto the embassy RCC config: the default
    /// 100 MHz preset emits the exact PLL fields + RANGE1 (the PLL panics in
    /// range 2), and an HSI-reset graph falls back to `Default::default()`.
    #[test]
    fn clock_graph_maps_to_embassy_rcc_config() {
        use crate::panels::mcu_module::clock::graph::{GraphClock, stm32wba_graph};
        use crate::panels::mcu_module::clock::model::ClockConfig;

        // 100 MHz preset (the shipped default selections).
        let gc = GraphClock {
            graph: stm32wba_graph(),
            layout: Default::default(),
            bindings: Default::default(),
        };
        let section =
            make_generated_section("WBA", &[], &ClockConfig::Graph(gc.clone()), "", false);
        for needle in [
            "config.rcc.hse = Some(rcc::Hse { prescaler: rcc::HsePrescaler::DIV1 });",
            "source: rcc::PllSource::HSE,",
            "prediv: rcc::PllPreDiv::DIV2,",
            "mul: rcc::PllMul::MUL25,",
            "divr: Some(rcc::PllDiv::DIV4),",
            "config.rcc.sys = rcc::Sysclk::PLL1_R;",
            "config.rcc.voltage_scale = rcc::VoltageScale::RANGE1;",
            "config.rcc.ahb_pre = rcc::AHBPrescaler::DIV1;",
            "config.rcc.apb7_pre = rcc::APBPrescaler::DIV1;",
            "let p = embassy_stm32::init(config);",
            "SYSCLK 100 MHz (HSE32 /2 x25 /4 via PLL1R)",
        ] {
            assert!(section.contains(needle), "missing: {needle}\n\n{section}");
        }

        // HSE directly as sysclk: no PLL block, but still RANGE1 + hse on.
        let mut gc2 = gc.clone();
        gc2.graph.node_mut("sw").unwrap().state =
            crate::panels::mcu_module::clock::graph::NodeState::Index(1);
        let s2 = make_generated_section("WBA", &[], &ClockConfig::Graph(gc2), "", false);
        assert!(s2.contains("config.rcc.sys = rcc::Sysclk::HSE;"));
        assert!(!s2.contains("config.rcc.pll1"));
        assert!(s2.contains("VoltageScale::RANGE1"));

        // Reset selections (HSI16, everything /1) → embassy's own default.
        let mut gc3 = gc;
        gc3.graph.node_mut("sw").unwrap().state =
            crate::panels::mcu_module::clock::graph::NodeState::Index(0);
        gc3.graph.node_mut("hse").unwrap().state =
            crate::panels::mcu_module::clock::graph::NodeState::Source {
                enabled: false,
                hz: 32_000_000,
            };
        let s3 = make_generated_section("WBA", &[], &ClockConfig::Graph(gc3), "", false);
        assert!(
            s3.contains("embassy_stm32::init(Default::default())"),
            "{s3}"
        );
    }

    /// Splice replaces only the GEN block, keeping the user tail.
    #[test]
    fn splice_preserves_user_tail() {
        let v1 = format!(
            "{}{}\n{USER_TAIL}",
            invariant_header("X", "x"),
            make_generated_section("X", &[], &ClockConfig::None, "", false)
        );
        let edited = v1.replace(
            "// Your main loop code here.",
            "// Your main loop code here.\n        my_custom();",
        );
        let pb5 = pin("PB5", PinFunction::GpioOutput);
        let v2 = splice_section(
            &edited,
            &make_generated_section("X", &[&pb5], &ClockConfig::None, "", false),
            "X",
            "x",
        );
        assert!(v2.contains("my_custom();")); // user edit survived
        assert!(v2.contains("Output::new(p.PB5")); // new pin present
    }
}
