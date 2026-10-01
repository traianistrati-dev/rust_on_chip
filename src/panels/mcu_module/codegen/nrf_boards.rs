//! Nordic's own development kits, generated from their pin tables.
//!
//! Like the pico2-ice: the `.ron` files in `assets/mcus/` are written by
//! [`definition`] and checked against it, so a pad changed by hand fails
//! `the_committed_definitions_are_the_tables` until the file is regenerated.
//!
//! Both kits are laid out the way they are used: the Arduino header's analog
//! side on the left, its digital side on the right, and the nets wired to the
//! kit's own LEDs, buttons, USB-serial bridge and memory along the top. Each
//! name leads with the nRF port and pin, which is what codegen reads.
//!
//! The functions follow the micro:bit's rules. Every signal routes to every
//! pin on an nRF52 (the PSEL registers hold a pin number), so the UARTEs and
//! the PWM blocks are on every general pad; SPI and I2C are offered only where
//! the header labels them, so autowire lands where a shield expects them. SPI
//! is SPIM2, the block no TWIM shares.

#[cfg(test)]
use super::nrf::{NrfChip, chip};
use crate::panels::mcu_module::builtins;
use crate::panels::mcu_module::clock::graph::config::GraphClock;
use crate::panels::mcu_module::clock::graph::model::{Edge, Node, NodeKind, NodeState};
use crate::panels::mcu_module::mcu_def::ClockDef;
#[cfg(test)]
use crate::panels::mcu_module::mcu_def::{McuDefinition, PinDef, PinLayout};
#[cfg(test)]
use crate::panels::mcu_module::pins::PinFunction;

/// What a pad carries.
#[cfg(test)]
#[derive(Clone, Copy)]
enum Pad {
    /// A general GPIO: `Gp(port, pin, note)`.
    Gp(u8, u8, &'static str),
    /// A GPIO wired to an on-board LED: an output, or PWM to dim it.
    Led(u8, u8, &'static str),
    /// A GPIO wired to an on-board button: an input.
    Button(u8, u8, &'static str),
    /// A GPIO the USB-serial bridge drives or reads, offered as that role of
    /// every UARTE (or as plain GPIO in the same direction).
    Uart(u8, u8, &'static str, Role),
    /// The SPI pads of the Arduino header: SPIM2's role, plus everything a
    /// general pad has.
    Spi(u8, u8, &'static str, Role),
    /// The I2C pads of the Arduino header: both TWIMs' role, plus the rest.
    I2c(u8, u8, &'static str, Role),
    /// Spoken for by the kit: reserved, explained by its name.
    Fixed(&'static str),
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum Role {
    Tx,
    Rx,
    Cts,
    Rts,
    Sck,
    Mosi,
    Miso,
    Sda,
    Scl,
}

#[cfg(test)]
use Pad::{Button, Fixed, Gp, I2c, Led, Spi, Uart};
#[cfg(test)]
use Role::*;

#[cfg(test)]
/// The SAADC input a pad is: the same eight pins on every nRF52 that has one.
fn ain(port: u8, pin: u8) -> Option<u8> {
    if port != 0 {
        return None;
    }
    match pin {
        2..=5 => Some(pin - 2),
        28..=31 => Some(pin - 24),
        _ => None,
    }
}

#[cfg(test)]
/// A general pad's functions on `c`: both directions, every role of every
/// UARTE, every channel of every PWM block, and its analog input if it has one.
fn general(c: &NrfChip, port: u8, pin: u8) -> Vec<PinFunction> {
    use PinFunction::*;
    let mut f = vec![GpioInput, GpioOutput];
    for &u in c.uarte {
        f.extend([UsartTx(u), UsartRx(u), UsartCts(u), UsartRts(u)]);
    }
    f.extend(pwm(c));
    if let Some(channel) = ain(port, pin).filter(|ch| c.ain.contains(ch)) {
        f.push(AdcChannel { adc: 0, channel });
    }
    f
}

#[cfg(test)]
fn pwm(c: &NrfChip) -> Vec<PinFunction> {
    c.pwm
        .iter()
        .flat_map(|&timer| (0..4).map(move |channel| PinFunction::TimerPwm { timer, channel }))
        .collect()
}

#[cfg(test)]
fn pin_def(c: &NrfChip, number: usize, pad: Pad) -> PinDef {
    use PinFunction::*;
    let name = |port: u8, pin: u8, note: &str| format!("P{port}.{pin:02} ({note})");
    let (name, reserved, functions) = match pad {
        Gp(port, pin, note) => (name(port, pin, note), false, general(c, port, pin)),
        Led(port, pin, note) => {
            let mut f = vec![GpioOutput];
            f.extend(pwm(c));
            (name(port, pin, note), false, f)
        }
        Button(port, pin, note) => (name(port, pin, note), false, vec![GpioInput]),
        Uart(port, pin, note, role) => {
            let (dir, role): (PinFunction, fn(u8) -> PinFunction) = match role {
                Tx => (GpioOutput, UsartTx),
                Rx => (GpioInput, UsartRx),
                Cts => (GpioInput, UsartCts),
                _ => (GpioOutput, UsartRts),
            };
            let mut f = vec![dir];
            f.extend(c.uarte.iter().map(|&u| role(u)));
            (name(port, pin, note), false, f)
        }
        Spi(port, pin, note, role) | I2c(port, pin, note, role) => {
            let mut f = general(c, port, pin);
            // After the UARTEs, before the PWM channels: the micro:bit's order.
            let at = 2 + 4 * c.uarte.len();
            let bus: Vec<PinFunction> = match role {
                Sck => vec![SpiSck(2)],
                Mosi => vec![SpiMosi(2)],
                Miso => vec![SpiMiso(2)],
                Sda => c.twim.iter().map(|&t| I2cSda(t)).collect(),
                _ => c.twim.iter().map(|&t| I2cScl(t)).collect(),
            };
            f.splice(at..at, bus);
            (name(port, pin, note), false, f)
        }
        Fixed(name) => (name.to_owned(), true, Vec::new()),
    };
    PinDef {
        number,
        name,
        reserved,
        functions,
        af: Vec::new(),
        fn_owner: Vec::new(),
    }
}

#[cfg(test)]
/// One kit: identity, chip, and its four rows.
struct Board {
    id: &'static str,
    display_name: &'static str,
    family: &'static str,
    left: &'static [Pad],
    right: &'static [Pad],
    top: &'static [Pad],
}

#[cfg(test)]
/// nRF52840 DK (PCA10056). Pin map from Nordic's PCA10056 hardware guide:
/// LEDs P0.13..16 and buttons P0.11/12/24/25 (all active low), the J-Link's
/// VCOM on P0.05..08, the 64 Mbit QSPI flash on P0.17/19..23, and the
/// 32.768 kHz crystal on P0.00/01 - fitted, so the clock tree offers LFXO.
const NRF52840_DK: Board = Board {
    id: "nrf52840_dk",
    display_name: "Nordic nRF52840 DK",
    family: "nrf52840",
    left: &[
        Gp(0, 3, "A0"),
        Gp(0, 4, "A1"),
        Gp(0, 28, "A2"),
        Gp(0, 29, "A3"),
        Gp(0, 30, "A4"),
        Gp(0, 31, "A5"),
        Gp(0, 2, "AREF"),
        Gp(1, 0, "SWO"),
        Gp(1, 9, ""),
        Gp(0, 9, "NFC1"),
        Gp(0, 10, "NFC2"),
    ],
    right: &[
        Gp(1, 1, "D0"),
        Gp(1, 2, "D1"),
        Gp(1, 3, "D2"),
        Gp(1, 4, "D3"),
        Gp(1, 5, "D4"),
        Gp(1, 6, "D5"),
        Gp(1, 7, "D6"),
        Gp(1, 8, "D7"),
        Gp(1, 10, "D8"),
        Gp(1, 11, "D9"),
        Gp(1, 12, "D10"),
        Spi(1, 13, "D11, MOSI", Mosi),
        Spi(1, 14, "D12, MISO", Miso),
        Spi(1, 15, "D13, SCK", Sck),
        I2c(0, 26, "SDA", Sda),
        I2c(0, 27, "SCL", Scl),
    ],
    top: &[
        Led(0, 13, "LED1, active LOW"),
        Led(0, 14, "LED2, active LOW"),
        Led(0, 15, "LED3, active LOW"),
        Led(0, 16, "LED4, active LOW"),
        Button(0, 11, "BUTTON1, active LOW"),
        Button(0, 12, "BUTTON2, active LOW"),
        Button(0, 24, "BUTTON3, active LOW"),
        Button(0, 25, "BUTTON4, active LOW"),
        Uart(0, 6, "VCOM TXD", Tx),
        Uart(0, 8, "VCOM RXD", Rx),
        Uart(0, 7, "VCOM CTS", Cts),
        Uart(0, 5, "VCOM RTS", Rts),
        Fixed("P0.17 (QSPI CS)"),
        Fixed("P0.19 (QSPI CLK)"),
        Fixed("P0.20 (QSPI IO0)"),
        Fixed("P0.21 (QSPI IO1)"),
        Fixed("P0.22 (QSPI IO2)"),
        Fixed("P0.23 (QSPI IO3)"),
        Fixed("P0.00 (XL1, 32.768 kHz)"),
        Fixed("P0.01 (XL2, 32.768 kHz)"),
        Fixed("P0.18 (RESET)"),
    ],
};

#[cfg(test)]
/// nRF52 DK (PCA10040), the nRF52832's kit - and the one Nordic hands out
/// for the nRF52810 and nRF52805 too. Pin map from the PCA10040 hardware
/// guide. Here the Arduino header and the kit SHARE pins: D2..D5 are the four
/// buttons and D6..D9 the four LEDs, so a pad says both.
const NRF52_DK: Board = Board {
    id: "nrf52832_dk",
    display_name: "Nordic nRF52 DK (nRF52832)",
    family: "nrf52832",
    left: &[
        Gp(0, 3, "A0"),
        Gp(0, 4, "A1"),
        Gp(0, 28, "A2"),
        Gp(0, 29, "A3"),
        Gp(0, 30, "A4"),
        Gp(0, 31, "A5"),
        Gp(0, 2, "AREF"),
        Gp(0, 9, "NFC1"),
        Gp(0, 10, "NFC2"),
    ],
    right: &[
        Gp(0, 11, "D0"),
        Gp(0, 12, "D1"),
        Gp(0, 13, "D2, BUTTON1"),
        Gp(0, 14, "D3, BUTTON2"),
        Gp(0, 15, "D4, BUTTON3"),
        Gp(0, 16, "D5, BUTTON4"),
        Gp(0, 22, "D10"),
        Spi(0, 23, "D11, MOSI", Mosi),
        Spi(0, 24, "D12, MISO", Miso),
        Spi(0, 25, "D13, SCK", Sck),
        I2c(0, 26, "SDA", Sda),
        I2c(0, 27, "SCL", Scl),
    ],
    top: &[
        Led(0, 17, "LED1, D6, active LOW"),
        Led(0, 18, "LED2, D7, SWO, active LOW"),
        Led(0, 19, "LED3, D8, active LOW"),
        Led(0, 20, "LED4, D9, active LOW"),
        Uart(0, 6, "VCOM TXD", Tx),
        Uart(0, 8, "VCOM RXD", Rx),
        Uart(0, 7, "VCOM CTS", Cts),
        Uart(0, 5, "VCOM RTS", Rts),
        Fixed("P0.00 (XL1, 32.768 kHz)"),
        Fixed("P0.01 (XL2, 32.768 kHz)"),
        Fixed("P0.21 (RESET)"),
    ],
};

#[cfg(test)]
const BOARDS: [&Board; 2] = [&NRF52840_DK, &NRF52_DK];

/// The nRF52 clock tree, with or without the 32.768 kHz crystal.
///
/// The micro:bit's tree is the base - no crystal fitted there - and LFXO
/// joins `lfclk_src` as its third input when the board has one.
/// `nrf::clock_choice` reads it by node id (`lfxo`), so the id is fixed.
pub(crate) fn clock_graph(lfxo: bool) -> GraphClock {
    let ClockDef::Graph(mut gc) = builtins::builtin_for("nrf52833_microbit_v2")
        .expect("built-in micro:bit v2")
        .clock
    else {
        unreachable!("the micro:bit carries its own graph");
    };
    if lfxo {
        let g = &mut gc.graph;
        let at = g
            .nodes
            .iter()
            .position(|n| n.id == "lfsynth")
            .expect("lfsynth node")
            + 1;
        g.nodes.insert(
            at,
            Node {
                id: "lfxo".into(),
                kind: NodeKind::Source {
                    min_hz: 32_768,
                    max_hz: 32_768,
                    gated: true,
                },
                state: NodeState::Source {
                    enabled: true,
                    hz: 32_768,
                },
                limit: None,
            },
        );
        if let Some(mux) = g.node_mut("lfclk_src") {
            mux.kind = NodeKind::Mux { inputs: 3 };
        }
        let at = g
            .edges
            .iter()
            .position(|e| e.to == "lfclk_src" && e.input == 1)
            .expect("lfsynth edge")
            + 1;
        g.edges.insert(
            at,
            Edge {
                from: "lfxo".into(),
                to: "lfclk_src".into(),
                input: 2,
            },
        );
    }
    gc
}

#[cfg(test)]
/// Point a definition at part `c`: family, CPU, and every project line the
/// generator reads - target, memory, probe-rs name, both HAL lines.
///
/// The New MCU form's Auto-fill writes the same fields from the same
/// `NrfChip` methods, so a definition the user makes for an nRF52810 gets
/// exactly the lines the harness here builds and cross-compiles.
pub(crate) fn apply_chip(def: &mut McuDefinition, c: &NrfChip) {
    def.family = c.family.into();
    def.cpu = c.cpu().into();
    def.max_mhz = Some(64);
    def.sram_kb = Some(c.ram_kb);
    let p = &mut def.project;
    p.target = c.target().into();
    p.flash_origin = "0x00000000".into();
    p.flash_size = format!("{}K", c.flash_kb);
    p.ram_origin = "0x20000000".into();
    p.ram_size = format!("{}K", c.ram_kb);
    p.hal_dep = c.hal_dep();
    p.hal_dep_async = Some(c.hal_dep_async());
    p.probe_chip = c.probe_chip();
}

#[cfg(test)]
fn definition_of(b: &Board) -> McuDefinition {
    let c = chip(b.family).expect("a known nRF52 part");
    let mut d = builtins::builtin_for("nrf52833_microbit_v2").expect("built-in micro:bit v2");
    d.id = b.id.into();
    d.display_name = b.display_name.into();
    d.package = "DK headers".into();
    d.board_chip = Some(c.part.into());
    apply_chip(&mut d, c);
    d.project.pkg_name = b.id.into();
    d.project.memory_comment = format!(
        "{} ({})  -  {} KiB Flash / {} KiB RAM",
        b.display_name, c.part, c.flash_kb, c.ram_kb
    );
    let mut next = 1;
    let mut row = |pads: &[Pad]| -> Vec<PinDef> {
        pads.iter()
            .map(|p| {
                next += 1;
                pin_def(c, next - 1, *p)
            })
            .collect()
    };
    d.pins = PinLayout {
        left: row(b.left),
        right: row(b.right),
        top: row(b.top),
        bottom: Vec::new(),
        grid: None,
    };
    d.clock = ClockDef::Graph(clock_graph(true));
    d
}

#[cfg(test)]
/// Every kit's definition, as the tables say.
pub(crate) fn definitions() -> Vec<McuDefinition> {
    BOARDS.iter().map(|b| definition_of(b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes the kits' definitions for `assets/mcus/`.
    ///
    /// ```text
    /// cargo test --bin rust_on_chip emit_nrf_dk_definitions -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "authoring tool: writes the nRF DK .ron files to the temp dir"]
    fn emit_nrf_dk_definitions() {
        for def in definitions() {
            let text = ron::ser::to_string_pretty(
                &def,
                ron::ser::PrettyConfig::default().struct_names(true),
            )
            .expect("serialise");
            let text = crate::panels::mcu_module::ron_text::bare_none(&text);
            let path = std::env::temp_dir().join(format!("{}.ron", def.id));
            std::fs::write(&path, text).expect("write");
            println!("wrote {}", path.display());
        }
    }

    /// The committed files are exactly what the tables say. A hand edit to a
    /// .ron, or a change to the micro:bit it borrows its clock from, fails
    /// here until the files are regenerated.
    #[test]
    fn the_committed_definitions_are_the_tables() {
        for want in definitions() {
            let have = builtins::builtin_for(&want.id)
                .unwrap_or_else(|| panic!("{} is not a built-in", want.id));
            assert!(
                have == want,
                "assets/mcus/{}.ron is stale: run emit_nrf_dk_definitions",
                want.id
            );
        }
    }

    use crate::panels::mcu_module::clock::model::ClockConfig;
    use crate::panels::mcu_module::mcu::Mcu;
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::pins::logic::pin::model::Edge as PinEdge;
    use crate::panels::mcu_module::project_gen;

    /// One project of the family harness.
    struct Case {
        dir: &'static str,
        board: &'static str,
        /// The part the kit runs, when not its own: the nRF52 DK is Nordic's
        /// kit for the 52810 and 52805 too, and the definition is retargeted
        /// the way the New MCU form's Auto-fill retargets one.
        part: Option<&'static str>,
        runtime: Runtime,
        /// The SPIM and TWIM instances wired on the header's SPI/I2C pads.
        spim: u8,
        twim: u8,
    }

    const CASES: [Case; 10] = [
        Case { dir: "eide_nrf52840_dk_check", board: "nrf52840_dk", part: None, runtime: Runtime::Blocking, spim: 2, twim: 0 },
        Case { dir: "eide_nrf52840_dk_async_check", board: "nrf52840_dk", part: None, runtime: Runtime::Async, spim: 2, twim: 0 },
        Case { dir: "eide_nrf52832_dk_check", board: "nrf52832_dk", part: None, runtime: Runtime::Blocking, spim: 2, twim: 1 },
        Case { dir: "eide_nrf52832_dk_async_check", board: "nrf52832_dk", part: None, runtime: Runtime::Async, spim: 2, twim: 1 },
        // SPIM0 and TWIM0 are SEPARATE blocks here (`SPI0`, `TWI0`).
        Case { dir: "eide_nrf52810_check", board: "nrf52832_dk", part: Some("nrf52810"), runtime: Runtime::Blocking, spim: 0, twim: 0 },
        Case { dir: "eide_nrf52810_async_check", board: "nrf52832_dk", part: Some("nrf52810"), runtime: Runtime::Async, spim: 0, twim: 0 },
        // No PWM at all, and the SAADC on AIN2/AIN3 only.
        Case { dir: "eide_nrf52805_async_check", board: "nrf52832_dk", part: Some("nrf52805"), runtime: Runtime::Async, spim: 0, twim: 0 },
        // TWIM0 shares `TWI0_SPI1` with SPIM1, not with SPIM0.
        Case { dir: "eide_nrf52811_check", board: "nrf52832_dk", part: Some("nrf52811"), runtime: Runtime::Blocking, spim: 0, twim: 0 },
        // No nrf-hal crate: Blocking is embassy-nrf without an executor.
        Case { dir: "eide_nrf52820_check", board: "nrf52832_dk", part: Some("nrf52820"), runtime: Runtime::Blocking, spim: 0, twim: 1 },
        Case { dir: "eide_nrf52820_async_check", board: "nrf52832_dk", part: Some("nrf52820"), runtime: Runtime::Async, spim: 0, twim: 1 },
    ];

    /// The case's definition and its wired `Mcu`: an LED, a PWM on two more
    /// LEDs, an armed button, the VCOM UART, the header's SPI and I2C, and
    /// A0..A2 on the SAADC, with the crystal clocks and the watchdog on.
    fn wired(case: &Case) -> (McuDefinition, Mcu) {
        let mut def = builtins::builtin_for(case.board).expect("built-in kit");
        if let Some(part) = case.part {
            apply_chip(&mut def, chip(part).expect("known part"));
            def.id = format!("{}_on_{}", part, case.board);
            def.display_name = format!("{} on the {}", chip(part).unwrap().part, def.display_name);
            def.project.pkg_name = def.id.clone();
        }
        let mut mcu = def.build_mcu();
        mcu.runtime = case.runtime;
        let pwm = |channel| PinFunction::TimerPwm { timer: 0, channel };
        let wire: [(&str, PinFunction); 11] = [
            ("(LED1", PinFunction::GpioOutput),
            ("(LED2", pwm(0)),
            ("(LED3", pwm(1)),
            ("BUTTON1", PinFunction::GpioInput),
            ("VCOM TXD", PinFunction::UsartTx(0)),
            ("VCOM RXD", PinFunction::UsartRx(0)),
            ("SCK)", PinFunction::SpiSck(case.spim)),
            ("MOSI)", PinFunction::SpiMosi(case.spim)),
            ("MISO)", PinFunction::SpiMiso(case.spim)),
            ("(SDA", PinFunction::I2cSda(case.twim)),
            ("(SCL", PinFunction::I2cScl(case.twim)),
        ];
        for p in mcu.iter_all_pins_mut() {
            if let Some((_, f)) = wire.iter().find(|(key, _)| p.name.contains(key)) {
                p.selected_function = f.clone();
                if p.name.contains("BUTTON1") {
                    p.irq = Some(PinEdge::Falling);
                }
            }
            // A0..A2 = AIN1, AIN2, AIN4: on the 52805 only AIN2 is real.
            for (key, channel) in [("(A0)", 1), ("(A1)", 2), ("(A2)", 4)] {
                if p.name.contains(key) {
                    p.selected_function = PinFunction::AdcChannel { adc: 0, channel };
                }
            }
        }
        mcu.reconcile_modules();
        for m in &mut mcu.modules {
            if let crate::panels::mcu_module::modules::ModuleConfig::Timer(c) = &mut m.config {
                c.freq_hz = 1_000;
                c.set_duty_x100(0, 2_500);
            }
        }
        let ClockConfig::Graph(gc) = &mut mcu.clock else {
            panic!("the kit carries a graph");
        };
        gc.graph.node_mut("hfclk_src").unwrap().state = NodeState::Index(1);
        gc.graph.node_mut("lfclk_src").unwrap().state = NodeState::Index(2);
        mcu.watchdog.nrf = Some(crate::panels::mcu_module::watchdog::NrfWdtConfig::default_for());
        (def, mcu)
    }

    /// Every case generates: the blocks the part lacks are comments, not code.
    #[test]
    fn every_case_generates_only_what_its_part_has() {
        for case in &CASES {
            let (_, mcu) = wired(case);
            let main = mcu.fresh_main_rs();
            let c = chip(&mcu.family).unwrap();
            assert!(main.contains("LfclkSource::ExternalXtal") || main.contains("set_lfclk_src_external"), "{}", case.dir);
            if c.pwm.is_empty() {
                assert!(main.contains("PWM0 is wired on"), "{}:\n{main}", case.dir);
                assert!(!main.contains("p.PWM0"), "{}", case.dir);
            }
            if c.ain.is_empty() {
                assert!(main.contains("AIN1 is wired on"), "{}:\n{main}", case.dir);
                assert!(!main.contains("SAADC"), "{}", case.dir);
            }
            if c.family == "nrf52805" {
                assert!(main.contains("AIN1 is wired on P0.03"), "{main}");
                assert!(main.contains("AIN4 is wired on P0.28"), "{main}");
            }
            if mcu.is_async() {
                assert!(main.contains("#[embassy_executor::main]"), "{}", case.dir);
            } else {
                assert!(main.contains("#[cortex_m_rt::entry]"), "{}", case.dir);
                assert!(!main.contains("embassy_executor"), "{}", case.dir);
            }
            if super::super::nrf::blocking_on_embassy(&mcu.family) && !mcu.is_async() {
                assert!(main.contains("let p = embassy_nrf::init(config);"), "{main}");
                assert!(main.contains("let mut twim1_ram = [0u8; 32];"), "{main}");
                assert!(!main.contains("static_cell"), "{main}");
                assert!(main.contains("| HAL: embassy-nrf (blocking)"), "{main}");
            }
        }
    }

    /// The small parts' serial blocks, by their embassy-nrf names.
    #[test]
    fn each_part_names_its_own_serial_blocks() {
        let main = |dir: &str| {
            let case = CASES.iter().find(|c| c.dir == dir).unwrap();
            wired(case).1.fresh_main_rs()
        };
        let m = main("eide_nrf52810_async_check");
        assert!(m.contains("p.SPI0,") && m.contains("p.TWI0,"), "{m}");
        let m = main("eide_nrf52820_async_check");
        assert!(m.contains("p.TWISPI0,") && m.contains("p.TWISPI1,"), "{m}");
        let m = main("eide_nrf52840_dk_async_check");
        assert!(m.contains("p.SPI2,") && m.contains("p.TWISPI0,"), "{m}");
    }

    /// Every case on disk, for a real cross-compile - written the way the
    /// app writes a project: `build_cfg` picks the runtime's HAL line, and
    /// on Async the same `ensure_*` calls `app.rs` makes add the executor.
    ///
    /// ```text
    /// cargo test --bin rust_on_chip emit_nrf_family_projects -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes projects to disk for a manual cross-compile"]
    fn emit_nrf_family_projects() {
        for case in &CASES {
            let (def, mcu) = wired(case);
            let main_rs = mcu.fresh_main_rs();
            let project = crate::panels::mcu_module::mcu_def::build_cfg(&def, Some(&mcu));
            let files = project_gen::build_project_files(&project, &def.toolchain, &main_rs);
            let user: Vec<(String, String)> = mcu.pin_tree_files();
            let dir = std::env::temp_dir().join(case.dir);
            let _ = std::fs::remove_dir_all(&dir);
            project_gen::write_project(&dir, &files, &user, &mcu.mcu_config_text(), "")
                .expect("write nrf project");
            if mcu.is_async() {
                let toml_path = dir.join("Cargo.toml");
                let toml = std::fs::read_to_string(&toml_path).expect("read Cargo.toml");
                let sources = [main_rs.as_str()];
                let toml = project_gen::ensure_async_deps(
                    &toml,
                    true,
                    project_gen::async_flavor_for(&mcu.family, ""),
                    false,
                    false,
                    false,
                    &sources,
                );
                let toml = project_gen::ensure_task_priority_deps(
                    &toml,
                    main_rs.contains("InterruptExecutor")
                        || super::super::nrf::needs_static_cell(&mcu),
                    &sources,
                );
                let toml = project_gen::ensure_m0_atomics(&toml, true, &project.target, &sources);
                std::fs::write(&toml_path, toml).expect("write Cargo.toml");
            }
            println!("wrote {}", dir.display());
            println!("target: {}", project.target);
        }
    }

    /// The micro:bit's tree IS the crystal-less one, so the two cannot drift.
    #[test]
    fn the_crystal_adds_one_source_and_one_mux_input() {
        use crate::panels::mcu_module::clock::graph::eval::evaluate;
        let without = clock_graph(false);
        let with = clock_graph(true);
        assert!(without.graph.node("lfxo").is_none());
        assert_eq!(with.graph.nodes.len(), without.graph.nodes.len() + 1);
        for lf in 0..3 {
            let mut g = with.graph.clone();
            g.node_mut("lfclk_src").unwrap().state = NodeState::Index(lf);
            assert_eq!(evaluate(&g)["lfclk"], 32_768, "lfclk_src={lf}");
        }
    }

    /// Every pad names a distinct nRF pin, the kit's own nets are reserved,
    /// and no pad offers a block its part does not have.
    #[test]
    fn every_pad_is_a_distinct_pin_of_a_block_the_part_has() {
        for def in definitions() {
            let c = chip(&def.family).unwrap();
            let mcu = def.build_mcu();
            let mut seen = std::collections::BTreeSet::new();
            let mut numbers = std::collections::BTreeSet::new();
            for p in mcu.iter_all_pins() {
                assert!(numbers.insert(p.number), "{}: pad {} twice", def.id, p.number);
                let pp = super::super::nrf::nrf_pin(&p.name)
                    .unwrap_or_else(|| panic!("{}: {} names no pin", def.id, p.name));
                assert!(seen.insert(pp), "{}: {:?} twice", def.id, pp);
                assert!(pp.0 == 0 || c.p1, "{}: {} on a part without P1", def.id, p.name);
                for f in &p.available_functions {
                    let ok = match f {
                        PinFunction::UsartTx(i)
                        | PinFunction::UsartRx(i)
                        | PinFunction::UsartCts(i)
                        | PinFunction::UsartRts(i) => c.has("uarte", *i),
                        PinFunction::SpiSck(i)
                        | PinFunction::SpiMosi(i)
                        | PinFunction::SpiMiso(i) => c.has("spim", *i),
                        PinFunction::I2cSda(i) | PinFunction::I2cScl(i) => c.has("twim", *i),
                        PinFunction::TimerPwm { timer, .. } => c.has("pwm", *timer),
                        PinFunction::AdcChannel { channel, .. } => c.ain.contains(channel),
                        _ => true,
                    };
                    assert!(ok, "{}: {} offers {f:?}", def.id, p.name);
                }
            }
        }
    }
}
