//! Nordic nRF52 family — `nrf52833-hal` on Blocking, `embassy-nrf` on Async.
//!
//! The definition (`assets/mcus/nrf52833_microbit_v2.ron`) is a BOARD, like the
//! Pico ones: the pads are the edge connector's, numbered as the silkscreen has
//! them, and each name leads with the nRF port and pin (`P0.21 (ROW1)`) so the
//! generated code can find the GPIO.
//!
//! Two facts about the silicon that this backend honors:
//!
//! - **Every signal routes to every pin.** There is no alternate-function
//!   table: a UARTE, SPIM, TWIM or PWM output is connected by writing the pin
//!   number into the peripheral's `PSEL` register. Every constructor therefore
//!   takes a DEGRADED pin (`Pin<Output<PushPull>>`, no pin number in the type),
//!   which is why the `init` signatures here name no pad. The definition still
//!   offers SPI and I2C only on the board's labeled pads, so autowire lands
//!   where accessories expect them.
//! - **SPIM0/SPIM1 share their peripheral IDs with TWIM0/TWIM1.** One block is
//!   either an SPI master or an I2C master, never both, which is why the
//!   definition offers SPI on SPIM2 and I2C on TWIM0/TWIM1.
//!
//! The clock block reads the Clock tab: which mux feeds HFCLK (the 64 MHz
//! internal oscillator, or the 32 MHz crystal doubled) and which feeds LFCLK
//! (the RC, synthesized from HFCLK, or a crystal when the tree has one). That
//! is the whole of what `nrf52833-hal`'s `Clocks` can set, and the whole of
//! what `embassy_nrf::config::Config` asks for too.
//!
//! The two runtimes share ONE header. A runtime switch re-splices only the
//! marked block and keeps everything above it, so the header's imports have to
//! resolve against either manifest; the Async block imports embassy-nrf's names
//! inside `main` instead.

use super::common::AUTOGEN_BANNER;
use super::common::{
    ASYNC_USER_TAIL, EdgeHook, GEN_BEGIN, GEN_END, USER_TAIL, blank_separated, edge_hook_name,
    mcu_id_marker_line, retarget_pristine_tail, var_suffix,
};
use super::family::FamilyBackend;
use crate::panels::mcu_module::mcu::Mcu;
use crate::panels::mcu_module::modules::UsartModuleConfig;
use crate::panels::mcu_module::pins::PinFunction;
use crate::panels::mcu_module::pins::logic::pin::{GpioMode, Pin};

pub struct NrfBackend;

/// What one nRF52 part has, as far as the generated code is concerned.
///
/// Read from the two HALs' own chip files (embassy-nrf 0.11 `src/chips/*.rs`,
/// nrf-hal-common 0.19 `spim.rs` / `twim.rs` / `uarte.rs` / `pwm.rs`), which
/// agree on every instance set below. A definition offers what its PADS can
/// carry; this says which blocks the SILICON has, so a definition cloned from
/// a bigger part cannot generate a constructor for a block that is not there.
#[derive(Debug)]
pub(crate) struct NrfChip {
    /// The family key, which is also embassy-nrf's feature name.
    pub family: &'static str,
    /// `nRF52840`, as Nordic writes it.
    pub part: &'static str,
    /// The nrf-hal crate, or `None` where nrf-hal has none - the nRF52820.
    /// Blocking then runs on embassy-nrf's blocking API instead.
    pub nrf_hal: Option<&'static str>,
    /// Cortex-M4 with or without the FPU: the four small parts have none.
    pub fpu: bool,
    pub flash_kb: u32,
    pub ram_kb: u32,
    /// NFCT: `nfc_pins` are antenna pins until the UICR (NFCT.PADCONFIG on
    /// the nRF54L) says otherwise, and
    /// embassy-nrf refuses `nfc-pins-as-gpio` on a part without them.
    pub nfc: bool,
    /// Port 1 exists. Read by the kits' own check that no pad names a pin
    /// the part does not have.
    #[cfg_attr(not(test), allow(dead_code))]
    pub p1: bool,
    pub uarte: &'static [u8],
    pub spim: &'static [u8],
    pub twim: &'static [u8],
    pub pwm: &'static [u8],
    /// The SAADC inputs (`AINn`), empty for a part without a SAADC. The
    /// 52805 has two: embassy-nrf implements AIN2 and AIN3 only.
    pub ain: &'static [u8],
    /// The full-speed USB device controller (USBD): 52820, 52833, 52840.
    pub usbd: bool,
    /// The QSPI flash controller: the 52840 alone.
    pub qspi: bool,
}

/// Every nRF52 part both HALs know, smallest first, then the nRF5340's
/// application core and the nRF54L15's.
pub(crate) const NRF52_CHIPS: [NrfChip; 9] = [
    NrfChip {
        family: "nrf52805",
        part: "nRF52805",
        nrf_hal: Some("nrf52805-hal"),
        fpu: false,
        flash_kb: 192,
        ram_kb: 24,
        nfc: false,
        p1: false,
        uarte: &[0],
        spim: &[0],
        twim: &[0],
        pwm: &[],
        ain: &[2, 3],
        usbd: false,
        qspi: false,
    },
    NrfChip {
        family: "nrf52810",
        part: "nRF52810",
        nrf_hal: Some("nrf52810-hal"),
        fpu: false,
        flash_kb: 192,
        ram_kb: 24,
        nfc: false,
        p1: false,
        uarte: &[0],
        spim: &[0],
        twim: &[0],
        pwm: &[0],
        ain: &[0, 1, 2, 3, 4, 5, 6, 7],
        usbd: false,
        qspi: false,
    },
    NrfChip {
        family: "nrf52811",
        part: "nRF52811",
        nrf_hal: Some("nrf52811-hal"),
        fpu: false,
        flash_kb: 192,
        ram_kb: 24,
        nfc: false,
        p1: false,
        uarte: &[0],
        spim: &[0, 1],
        twim: &[0],
        pwm: &[0],
        ain: &[0, 1, 2, 3, 4, 5, 6, 7],
        usbd: false,
        qspi: false,
    },
    NrfChip {
        family: "nrf52820",
        part: "nRF52820",
        nrf_hal: None,
        fpu: false,
        flash_kb: 256,
        ram_kb: 32,
        nfc: false,
        p1: false,
        uarte: &[0],
        spim: &[0, 1],
        twim: &[0, 1],
        pwm: &[],
        ain: &[],
        usbd: true,
        qspi: false,
    },
    NrfChip {
        family: "nrf52832",
        part: "nRF52832",
        nrf_hal: Some("nrf52832-hal"),
        fpu: true,
        flash_kb: 512,
        ram_kb: 64,
        nfc: true,
        p1: false,
        uarte: &[0],
        spim: &[0, 1, 2],
        twim: &[0, 1],
        pwm: &[0, 1, 2],
        ain: &[0, 1, 2, 3, 4, 5, 6, 7],
        usbd: false,
        qspi: false,
    },
    NrfChip {
        family: "nrf52833",
        part: "nRF52833",
        nrf_hal: Some("nrf52833-hal"),
        fpu: true,
        flash_kb: 512,
        ram_kb: 128,
        nfc: true,
        p1: true,
        uarte: &[0, 1],
        spim: &[0, 1, 2, 3],
        twim: &[0, 1],
        pwm: &[0, 1, 2, 3],
        ain: &[0, 1, 2, 3, 4, 5, 6, 7],
        usbd: true,
        qspi: false,
    },
    NrfChip {
        family: "nrf52840",
        part: "nRF52840",
        nrf_hal: Some("nrf52840-hal"),
        fpu: true,
        flash_kb: 1024,
        ram_kb: 256,
        nfc: true,
        p1: true,
        uarte: &[0, 1],
        spim: &[0, 1, 2, 3],
        twim: &[0, 1],
        pwm: &[0, 1, 2, 3],
        ain: &[0, 1, 2, 3, 4, 5, 6, 7],
        usbd: true,
        qspi: true,
    },
    // The APPLICATION core of the nRF5340, secure (`nrf5340-app-s`): a
    // Cortex-M33 at up to 128 MHz. UARTE/SPIM/TWIM n share SERIALn, SPIM4 is
    // its own 32 MHz block, and the network core is not this definition's.
    // nrf-hal's `nrf5340-app-hal` exists but shares none of the nRF52 calls
    // this backend emits, so Blocking is embassy-nrf, as on the 52820.
    NrfChip {
        family: "nrf5340",
        part: "nRF5340",
        nrf_hal: None,
        fpu: true,
        flash_kb: 1024,
        ram_kb: 512,
        nfc: true,
        p1: true,
        uarte: &[0, 1, 2, 3],
        spim: &[0, 1, 2, 3, 4],
        twim: &[0, 1, 2, 3],
        pwm: &[0, 1, 2, 3],
        ain: &[0, 1, 2, 3, 4, 5, 6, 7],
        usbd: true,
        qspi: true,
    },
    // The nRF54L15's application core, secure (`nrf54l15-app-s`): a Cortex-M33
    // at 128 MHz with 1524 KiB of RRAM. Instances are numbered by POWER
    // DOMAIN - SERIAL00 is the fast one, SERIAL20/21/22 and SERIAL30 the rest -
    // so the numbers here are 0, 20, 21, 22 and 30, and the PWMs 20..22. GPIO
    // has three ports. No nrf-hal crate, no USB, no QSPI; embassy-time runs on
    // the GRTC. Instance 0 is UARTE00 / SPIM00 only: there is no TWIM00.
    NrfChip {
        family: "nrf54l15",
        part: "nRF54L15",
        nrf_hal: None,
        fpu: true,
        flash_kb: 1524,
        ram_kb: 256,
        nfc: true,
        p1: true,
        uarte: &[0, 20, 21, 22, 30],
        spim: &[0, 20, 21, 22, 30],
        twim: &[20, 21, 22, 30],
        pwm: &[20, 21, 22],
        ain: &[0, 1, 2, 3, 4, 5, 6, 7],
        usbd: false,
        qspi: false,
    },
];

/// The part `family` names, or `None` for anything that is not an nRF52.
pub(crate) fn chip(family: &str) -> Option<&'static NrfChip> {
    NRF52_CHIPS.iter().find(|c| c.family == family)
}

impl NrfChip {
    /// The nRF5340's application core, where the nRF52 answers stop holding.
    pub fn nrf53(&self) -> bool {
        self.family == "nrf5340"
    }

    /// The nRF54L15, where they stop holding again, differently.
    pub fn nrf54(&self) -> bool {
        self.family == "nrf54l15"
    }

    /// Whether block instance `inst` can reach a pin on `port` - the SAFE
    /// answer the built-in kits are generated from.
    ///
    /// On an nRF52 or the nRF5340 any signal routes to any pin. On the nRF54L
    /// each block lives in a power domain and reaches that domain's port:
    /// SERIAL00 is on P2, SERIAL20/21/22 and PWM20/21/22 on P1, SERIAL30 on P0.
    /// The datasheet allows more and less than this at the edges - SERIAL00
    /// only on its dedicated P2 pins, SERIAL20/21 also on a few P2 pins in
    /// Constant Latency mode - which `nrf54_pin_rule` checks per signal.
    /// Read by the kits' generator (an authoring tool, so tests only).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn reaches(&self, inst: u8, port: u8) -> bool {
        !self.nrf54()
            || match port {
                0 => inst == 30,
                1 => (20..=22).contains(&inst),
                _ => inst == 0,
            }
    }

    /// The embassy-time driver: RTC1 on the nRF52 and nRF5340, the GRTC on
    /// the nRF54L, which has no RTC1 to give.
    pub fn time_driver(&self) -> &'static str {
        if self.nrf54() {
            "time-driver-grtc"
        } else {
            "time-driver-rtc1"
        }
    }

    /// embassy-nrf's feature for the part: the family key on an nRF52, the
    /// SECURE application core on the nRF5340 - bare metal, no TF-M.
    pub fn embassy_feature(&self) -> &'static str {
        if self.nrf53() {
            "nrf5340-app-s"
        } else if self.nrf54() {
            "nrf54l15-app-s"
        } else {
            self.family
        }
    }

    /// The watchdog's `Peripherals` field: the nRF5340 has two, and the
    /// application core's own is WDT0.
    pub fn wdt(&self) -> &'static str {
        // The nRF54L names it WDT0 too on the SECURE core (WDT31 underneath);
        // its `WDT` alias exists on the non-secure one only.
        if self.nrf53() || self.nrf54() { "WDT0" } else { "WDT" }
    }

    /// The two NFC antenna pins.
    pub fn nfc_pins(&self) -> [(u8, u8); 2] {
        if self.nrf53() {
            [(0, 2), (0, 3)]
        } else if self.nrf54() {
            [(1, 2), (1, 3)]
        } else {
            [(0, 9), (0, 10)]
        }
    }

    /// The SAADC input a pin is, or `None`. The nRF52 parts put AIN0..7 on
    /// P0.02..05 and P0.28..31; the nRF5340 on P0.04..07 and P0.25..28.
    /// Read by the kits' generator, which is an authoring tool (tests only).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn ain_of(&self, (port, pin): (u8, u8)) -> Option<u8> {
        if self.nrf54() {
            // AIN0..7 are P1.04..07 and P1.11..14 on the nRF54L15.
            let ch = match (port, pin) {
                (1, 4..=7) => pin - 4,
                (1, 11..=14) => pin - 7,
                _ => return None,
            };
            return self.ain.contains(&ch).then_some(ch);
        }
        if port != 0 {
            return None;
        }
        let ch = if self.nrf53() {
            match pin {
                4..=7 => pin - 4,
                25..=28 => pin - 21,
                _ => return None,
            }
        } else {
            match pin {
                2..=5 => pin - 2,
                28..=31 => pin - 24,
                _ => return None,
            }
        };
        self.ain.contains(&ch).then_some(ch)
    }

    /// The vector USB's VBUS detection binds: the power block on an nRF52,
    /// the USB regulator on the nRF5340.
    pub fn vbus_irq(&self) -> &'static str {
        if self.nrf53() { "USBREGULATOR" } else { "CLOCK_POWER" }
    }

    /// The Rust target: Cortex-M4F is hard-float, the M4 without an FPU is
    /// not, and the Cortex-M33 of the nRF5340 and nRF54L15 is Armv8-M
    /// Mainline.
    pub fn target(&self) -> &'static str {
        if self.nrf53() || self.nrf54() {
            "thumbv8m.main-none-eabihf"
        } else if self.fpu {
            "thumbv7em-none-eabihf"
        } else {
            "thumbv7em-none-eabi"
        }
    }

    pub fn cpu(&self) -> &'static str {
        if self.nrf53() || self.nrf54() {
            "ARM Cortex-M33"
        } else if self.fpu {
            "ARM Cortex-M4F"
        } else {
            "ARM Cortex-M4"
        }
    }

    /// probe-rs's name for the part. Every nRF52 is `_xxAA` there; the
    /// 52832's 256 KiB `_xxAB` is the one variant, and its flash is a
    /// definition's to state.
    pub fn probe_chip(&self) -> String {
        format!("{}_xxAA", self.part)
    }

    /// The Blocking runtime's dependency line: the part's nrf-hal crate, or
    /// embassy-nrf with no time driver where nrf-hal has no crate for it.
    pub fn hal_dep(&self) -> String {
        match self.nrf_hal {
            Some(krate) => format!(
                "{krate} = {{ version = \"0.19\", default-features = false, features = [\"rt\"] }}"
            ),
            None => format!(
                "embassy-nrf = {{ version = \"0.11\", features = [\"{}\"] }}",
                self.embassy_feature()
            ),
        }
    }

    /// The Async runtime's dependency line. `nfc-pins-as-gpio` only on a part
    /// with NFCT: embassy-nrf has a `compile_error!` for it anywhere else.
    pub fn hal_dep_async(&self) -> String {
        let nfc = if self.nfc {
            ", \"nfc-pins-as-gpio\""
        } else {
            ""
        };
        format!(
            "embassy-nrf = {{ version = \"0.11\", features = [\"{}\", \"{}\", \"gpiote\"{nfc}] }}",
            self.embassy_feature(),
            self.time_driver()
        )
    }

    /// Whether the part has `kind` block `n` (`"uarte"`, `"spim"`, `"twim"`,
    /// `"pwm"`).
    pub fn has(&self, kind: &str, n: u8) -> bool {
        let set = match kind {
            "uarte" => self.uarte,
            "spim" => self.spim,
            "twim" => self.twim,
            "pwm" => self.pwm,
            _ => return false,
        };
        set.contains(&n)
    }
}

/// Whether `family` is one of Nordic's nRF52 parts.
///
/// A list, not a prefix: the parts differ in which blocks they have and in
/// which crate drives them, so a key this backend has no row for would be
/// generated against guesses.
pub fn is_nrf(family: &str) -> bool {
    chip(family).is_some()
}

/// `nrf52833_hal` — the nrf-hal crate as it is written in Rust, or `None`
/// for the part nrf-hal has no crate for.
fn hal_crate(family: &str) -> Option<String> {
    chip(family)?.nrf_hal.map(|c| c.replace('-', "_"))
}

/// Whether the Blocking runtime on `family` is embassy-nrf used without an
/// executor: the nRF52820, which nrf-hal has no crate for, and the nRF5340,
/// whose nrf-hal crate shares none of the calls this backend emits.
pub fn blocking_on_embassy(family: &str) -> bool {
    chip(family).is_some_and(|c| c.nrf_hal.is_none())
}

/// A GPIO as (port, pin): `P0.21 (ROW1)` -> `(0, 21)`.
///
/// `pub(crate)` for the same reason `rp::gpio_index` is: anything else that
/// has to name the pin a pad carries must parse it the way the emitter does.
pub(crate) fn nrf_pin(name: &str) -> Option<(u8, u8)> {
    let head = name.split_whitespace().next()?;
    let (port, pin) = head.strip_prefix('P')?.split_once('.')?;
    let (port, pin): (u8, u8) = (port.parse().ok()?, pin.parse().ok()?);
    (port <= 2 && pin < 32).then_some((port, pin))
}

/// `P0.21` — how the generated comments name a pin.
fn label((port, pin): (u8, u8)) -> String {
    format!("P{port}.{pin:02}")
}

/// `p0_21` — the identifier a pin's binding is built from.
fn ident((port, pin): (u8, u8)) -> String {
    format!("p{port}_{pin:02}")
}

/// `port0.p0_21` — the field of the `Parts` struct that owns the pin.
fn field((port, pin): (u8, u8)) -> String {
    format!("port{port}.p{port}_{pin:02}")
}

/// The board's own name for a pad, for comments: `P0.21 (ROW1)` -> `ROW1`.
fn board_name(name: &str) -> Option<&str> {
    let start = name.find('(')? + 1;
    let end = name.rfind(')')?;
    (start < end).then(|| name[start..end].trim())
}

/// A pad the user wired but has not written code for yet is not a mistake -
/// it is a pad they are about to use. Same words the F1 and RP backends use.
const ALLOW: &str = "    #[allow(unused_mut, unused_variables)]
";

// ── Clock tab ───────────────────────────────────────────────────────────────

/// What the tree chose for the two clocks.
struct ClockChoice {
    /// HFCLK from the 32 MHz crystal (doubled) rather than the internal 64 MHz
    /// oscillator.
    hfxo: bool,
    lf: LfSource,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LfSource {
    Rc,
    Synth,
    Xtal,
}

/// The node feeding mux `mux` at the input its state selects.
fn mux_source(mcu: &Mcu, mux: &str) -> Option<String> {
    use crate::panels::mcu_module::clock::graph::model::NodeState;
    use crate::panels::mcu_module::clock::model::ClockConfig;
    let ClockConfig::Graph(gc) = &mcu.clock else {
        return None;
    };
    let idx = match gc.graph.node(mux).map(|n| &n.state) {
        Some(NodeState::Index(i)) => *i,
        _ => return None,
    };
    gc.graph
        .edges
        .iter()
        .find(|e| e.to == mux && e.input == idx)
        .map(|e| e.from.clone())
}

/// Read off the tree by following each mux's selected edge back to its
/// source, so the choice survives a node being renamed in the editor as long
/// as the mux keeps its id. A tree with no mux (or no tree) means the reset
/// defaults: internal oscillator and internal RC.
fn clock_choice(mcu: &Mcu) -> ClockChoice {
    let hf = mux_source(mcu, "hfclk_src");
    let lf = mux_source(mcu, "lfclk_src");
    ClockChoice {
        hfxo: hf.as_deref().is_some_and(|s| s.starts_with("hfxo")),
        lf: match lf.as_deref() {
            Some("lfsynth") => LfSource::Synth,
            Some("lfxo") => LfSource::Xtal,
            _ => LfSource::Rc,
        },
    }
}

fn clock_lines(mcu: &Mcu, hal: &str) -> String {
    let c = clock_choice(mcu);
    let mut o = String::new();
    o.push_str("    // From the Clock tab. HFCLK is 64 MHz either way; the choice is whether\n");
    o.push_str("    // the 32 MHz crystal is started, which the radio and USB need. LFCLK is\n");
    o.push_str("    // 32.768 kHz from whichever source the tree selects.\n");
    o.push_str(&format!(
        "    let clocks = {hal}::clocks::Clocks::new(p.CLOCK)\n"
    ));
    if c.hfxo {
        o.push_str("        .enable_ext_hfosc()\n");
    } else if usb_wired(mcu) {
        // Not a preference: `UsbPeripheral::new` takes `&Clocks<ExternalOscillator, ..>`,
        // so the crystal is a type the USB bus cannot be built without.
        o.push_str("        // The USB module needs the crystal: the Clock tab's internal choice is\n        // overridden, since USB cannot run from the RC oscillator.\n");
        o.push_str("        .enable_ext_hfosc()\n");
    }
    match c.lf {
        LfSource::Rc => o.push_str("        .set_lfclk_src_rc()\n"),
        LfSource::Synth => o.push_str("        .set_lfclk_src_synth()\n"),
        LfSource::Xtal => o.push_str(&format!(
            "        .set_lfclk_src_external({hal}::clocks::LfOscConfiguration::NoExternalNoBypass)\n"
        )),
    }
    o.push_str("        .start_lfclk();\n");
    o.push_str("    let _ = &clocks;\n\n");
    o
}

// ── GPIO ────────────────────────────────────────────────────────────────────

fn gpio_lines(mcu: &Mcu, hal: &str) -> String {
    let mut pins_out: Vec<String> = Vec::new();
    for p in mcu.iter_all_pins().filter(|p| !p.reserved) {
        let Some(pp) = nrf_pin(&p.name) else {
            continue;
        };
        let sfx = var_suffix(&p.selected_function);
        let var = format!("{}_{sfx}", ident(pp));
        let note = board_name(&p.name).map_or(String::new(), |b| format!(" ({b})"));
        match p.selected_function {
            PinFunction::GpioOutput => {
                // `for_output` / `for_input`: the default is the FIRST mode the
                // panel offers (push-pull, floating), and a mode left over from
                // the other direction is ignored rather than misread.
                let ctor = match GpioMode::for_output(p.io_mode) {
                    GpioMode::OpenDrain => format!(
                        "into_open_drain_output({hal}::gpio::OpenDrainConfig::Standard0Disconnect1, {hal}::gpio::Level::High)"
                    ),
                    _ => format!("into_push_pull_output({hal}::gpio::Level::Low)"),
                };
                pins_out.push(format!(
                    "    // {}{note}\n{ALLOW}    let mut {var} = {}.{ctor};\n",
                    label(pp),
                    field(pp)
                ));
            }
            PinFunction::GpioInput => {
                let ctor = match GpioMode::for_input(p.io_mode) {
                    GpioMode::PullUp => "into_pullup_input()",
                    GpioMode::PullDown => "into_pulldown_input()",
                    _ => "into_floating_input()",
                };
                let mut entry = format!(
                    "    // {}{note}\n{ALLOW}    let {var} = {}.{ctor};\n",
                    label(pp),
                    field(pp)
                );
                // The edge is set in the pin panel on every runtime. Nothing
                // here acts on it yet, and saying so beats dropping it.
                if p.irq.is_some() {
                    entry.push_str(&format!(
                        "    // {} is armed for an interrupt, which this Blocking project does\n    // not generate: poll it, or wire GPIOTE by hand.\n",
                        label(pp)
                    ));
                }
                pins_out.push(entry);
            }
            _ => {}
        }
    }
    blank_separated(pins_out)
}

/// The NFC pads, if anything is wired to them.
///
/// The definition offers them as GPIO because they ARE GPIO once the UICR
/// says so, and the HAL cannot say so at run time. Left unsaid, a button on
/// pad 8 reads as a dead input with no error anywhere.
fn nfc_note(mcu: &Mcu) -> String {
    let used = nfc_pads_used(mcu);
    if used.is_empty() {
        return String::new();
    }
    format!(
        "    // {} {} the NFC antenna pins, and stay NFC until bit 0 of the UICR's\n    // NFCPINS register (0x1000120C) is cleared. The UICR is flash, written\n    // through the NVMC, and nrf-hal has no helper: clear it once from code\n    // (NVMC.CONFIG = WEN, write 0xFFFFFFFE to 0x1000120C, wait for NVMC.READY,\n    // CONFIG = REN, then reset). A debug probe can run the same sequence, but a\n    // lone write to the UICR without CONFIG = WEN first is ignored.\n",
        used.join(" and "),
        if used.len() == 1 { "is one of" } else { "are" }
    )
}

/// The NFC pads anything is wired to, as `P0.09` labels. None on a part
/// without NFCT, where P0.09/P0.10 are ordinary GPIO from reset.
fn nfc_pads_used(mcu: &Mcu) -> Vec<String> {
    // The two antenna pins - P0.09/P0.10 on an nRF52, P0.02/P0.03 on the
    // nRF5340 - are NFC until the UICR's `NFCPINS` register is cleared, and
    // the UICR is flash: nothing this code emits at run time can change it.
    // The nRF54L's P1.02/P1.03 are the exception: NFCT.PADCONFIG, a plain
    // register, written by embassy-nrf's `init` at every boot.
    let Some(nfc) = chip(&mcu.family).filter(|c| c.nfc).map(|c| c.nfc_pins()) else {
        return Vec::new();
    };
    mcu.iter_all_pins()
        .filter(|p| !p.reserved && p.selected_function != PinFunction::Unset)
        .filter_map(|p| nrf_pin(&p.name))
        .filter(|pp| nfc.contains(pp))
        .map(label)
        .collect()
}

// ── Buses ───────────────────────────────────────────────────────────────────

/// Whether the part has block `kind` instance `n` (`None` for the SAADC,
/// which has no number). An unknown family answers yes: nothing to check
/// against, and `is_nrf` already keeps such a family out of this backend.
fn block_present(mcu: &Mcu, kind: &str, n: Option<u8>) -> bool {
    let Some(c) = chip(&mcu.family) else {
        return true;
    };
    match (kind, n) {
        ("saadc", Some(ch)) => c.ain.contains(&ch),
        ("saadc", None) => !c.ain.is_empty(),
        ("usbd", _) => c.usbd,
        ("qspi", _) => c.qspi,
        (k, Some(n)) => c.has(k, n),
        _ => true,
    }
}

/// The block a pin function belongs to: `("spim", Some(2), "SPIM2")`.
fn block_of(f: &PinFunction) -> Option<(&'static str, Option<u8>, String)> {
    Some(match f {
        PinFunction::UsartTx(i)
        | PinFunction::UsartRx(i)
        | PinFunction::UsartCts(i)
        | PinFunction::UsartRts(i) => ("uarte", Some(*i), format!("UARTE{i}")),
        PinFunction::SpiSck(i) | PinFunction::SpiMosi(i) | PinFunction::SpiMiso(i) => {
            ("spim", Some(*i), format!("SPIM{i}"))
        }
        PinFunction::I2cSda(i) | PinFunction::I2cScl(i) => ("twim", Some(*i), format!("TWIM{i}")),
        PinFunction::TimerPwm { timer, .. } => ("pwm", Some(*timer), format!("PWM{timer}")),
        PinFunction::AdcChannel { channel, .. } => ("saadc", Some(*channel), format!("AIN{channel}")),
        PinFunction::UsbDm | PinFunction::UsbDp => ("usbd", None, "USBD".to_owned()),
        PinFunction::QspiClk | PinFunction::QspiNcs { .. } | PinFunction::QspiIo { .. } => {
            ("qspi", None, "QSPI".to_owned())
        }
        _ => return None,
    })
}

/// A comment per wired block the part does not have.
///
/// A definition cloned from a bigger part keeps offering its blocks - a pad
/// of an nRF52-DK running an nRF52810 still lists SPIM2 - and a constructor
/// for a block the PAC does not have is a compile error in the user's
/// project. Every emitter drops such a signal; this says so where it would
/// have been built.
fn missing_block_notes(mcu: &Mcu) -> String {
    let Some(c) = chip(&mcu.family) else {
        return String::new();
    };
    let mut missing: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for p in mcu.iter_all_pins().filter(|p| !p.reserved) {
        let Some((kind, n, name)) = block_of(&p.selected_function) else {
            continue;
        };
        if !block_present(mcu, kind, n) {
            // The USB pads are no GPIO, so they are named as the pad is.
            let pad = nrf_pin(&p.name).map_or_else(|| p.name.clone(), label);
            missing.entry(name).or_default().push(pad);
        }
    }
    let mut o = String::new();
    for (name, mut pads) in missing {
        pads.sort_unstable();
        o.push_str(&format!(
            "    // {name} is wired on {}, but the {} has no {name}: it is not built.\n",
            pads.join(" and "),
            c.part
        ));
    }
    o
}

// ── USB and QSPI ────────────────────────────────────────────────────────────

/// Whether the USB device is built: both pads wired, on a part with a USBD.
///
/// The pads are the chip's dedicated D+/D- balls, not GPIO, so nothing about
/// them reaches a constructor: wiring them is how the user asks for the
/// controller.
fn usb_wired(mcu: &Mcu) -> bool {
    let wired = |f: PinFunction| {
        mcu.iter_all_pins()
            .any(|p| !p.reserved && p.selected_function == f)
    };
    block_present(mcu, "usbd", None) && wired(PinFunction::UsbDp) && wired(PinFunction::UsbDm)
}

/// `(sck, csn, [io0..io3])`, each as `(port, pin)`.
type QspiPads = ((u8, u8), (u8, u8), [(u8, u8); 4]);

/// The QSPI's six pads as `(sck, csn, [io0..io3])`, when every one is wired on
/// a part with a QSPI. The nRF driver is quad-only: its one constructor takes
/// all six, so a bank short of a lane builds nothing.
fn qspi_wired(mcu: &Mcu) -> Option<QspiPads> {
    if !block_present(mcu, "qspi", None) {
        return None;
    }
    let pad = |want: &PinFunction| {
        mcu.iter_all_pins()
            .filter(|p| !p.reserved && p.selected_function == *want)
            .find_map(|p| nrf_pin(&p.name))
    };
    let io = |lane| pad(&PinFunction::QspiIo { bank: 1, lane });
    Some((
        pad(&PinFunction::QspiClk)?,
        pad(&PinFunction::QspiNcs { bank: 1 })?,
        [io(0)?, io(1)?, io(2)?, io(3)?],
    ))
}

/// Whether any QSPI pad is wired at all - to say what is missing when
/// [`qspi_wired`] cannot build it.
fn qspi_touched(mcu: &Mcu) -> bool {
    mcu.iter_all_pins().any(|p| {
        !p.reserved
            && matches!(
                p.selected_function,
                PinFunction::QspiClk | PinFunction::QspiNcs { .. } | PinFunction::QspiIo { .. }
            )
    })
}

/// The USB crates an nRF project needs, as `(usb-device stack, embassy-usb)`.
///
/// Asked by `app.rs` when it writes `Cargo.toml` and by the harness that
/// cross-compiles the same project, so the two cannot disagree. nrf-hal's
/// `Usbd` is a `usb-device` 0.3 bus - the ESP OTG stack's versions - and
/// embassy-nrf's driver runs under `embassy-usb`. The part nrf-hal has no
/// crate for builds no USB on Blocking (embassy-usb needs an executor).
pub fn usb_stack(mcu: &Mcu) -> (bool, bool) {
    if !is_nrf(&mcu.family) || !usb_wired(mcu) {
        return (false, false);
    }
    if mcu.is_async() {
        (false, true)
    } else {
        (hal_crate(&mcu.family).is_some(), false)
    }
}

/// The module's VID / PID / product, as three consts at the top of `main`.
fn usb_identity(mcu: &Mcu) -> String {
    let d = crate::panels::mcu_module::modules::UsbModuleConfig::new(1);
    let cfgs = crate::panels::mcu_module::modules::usb_configs(&mcu.modules);
    let c = cfgs.values().next().unwrap_or(&d);
    format!(
        "    // From the USB module. `0x16c0:0x27dd` is pid.codes' test pair - fine on\n    // a bench, not for anything shipped.\n    const USB_VID: u16 = 0x{:04x};\n    const USB_PID: u16 = 0x{:04x};\n    const USB_PRODUCT: &str = {:?};\n",
        c.vid, c.pid, c.product
    )
}

/// The QSPI bus clock for the module's prescaler: 32 MHz / (prescaler + 1),
/// as `(Hz, Frequency variant)`. The block's divider runs 1..=16.
fn qspi_frequency(prescaler: u8) -> (u32, &'static str) {
    const F: [&str; 16] = [
        "M32", "M16", "M10_7", "M8", "M6_4", "M5_3", "M4_6", "M4", "M3_6", "M3_2", "M2_9",
        "M2_7", "M2_5", "M2_3", "M2_1", "M2",
    ];
    let i = usize::from(prescaler).min(F.len() - 1);
    (32_000_000 / (i as u32 + 1), F[i])
}

/// The nRF54L15's clock pins in the QFN48 package (the nRF54L15 DK's): the
/// only pins SPIM SCK and TWIM SCL may use. From the datasheet's QFN48 pin
/// assignment table; other packages differ.
const NRF54_CLOCK_PINS: [(u8, u8); 9] = [
    (0, 3),
    (0, 4),
    (1, 3),
    (1, 4),
    (1, 8),
    (1, 11),
    (1, 12),
    (2, 1),
    (2, 6),
];

/// What a pin function does on its serial block, for the nRF54L pin rules.
fn serial_role(f: &PinFunction) -> Option<(u8, &'static str)> {
    Some(match f {
        PinFunction::SpiSck(i) => (*i, "sck"),
        PinFunction::SpiMosi(i) => (*i, "mosi"),
        PinFunction::SpiMiso(i) => (*i, "miso"),
        PinFunction::UsartTx(i) => (*i, "txd"),
        PinFunction::UsartRx(i) => (*i, "rxd"),
        PinFunction::UsartCts(i) => (*i, "cts"),
        PinFunction::UsartRts(i) => (*i, "rts"),
        PinFunction::I2cScl(i) => (*i, "scl"),
        PinFunction::I2cSda(i) => (*i, "sda"),
        _ => return None,
    })
}

/// The dedicated P2 pin of a SPIM/UARTE `role` in the SERIAL20 (P2.00..05)
/// or SERIAL21 (P2.06..10) group - the nRF54L15 datasheet's QFN48 table.
/// SERIAL00 may use either group's pin; SERIAL20/21 their own, cross-domain.
/// No TWIM has a P2 pin at all.
fn nrf54_p2_pin(role: &str, group: u8) -> Option<u8> {
    let (g20, g21) = match role {
        "sck" => (1, 6),
        "mosi" | "txd" => (2, 8),
        "miso" | "cts" => (4, 9),
        "rxd" => (0, 7),
        "rts" => (5, 10),
        _ => return None,
    };
    match group {
        20 => Some(g20),
        21 => Some(g21),
        _ => None,
    }
}

/// What is wrong with `f` on pin `pp` of an nRF54L15, or `None`.
///
/// Every case here COMPILES - PSEL takes any pin number - and then the pin
/// never moves, or moves only in a power mode nothing started. That is why
/// the generated code says so rather than leaving it to the scope.
fn nrf54_pin_rule(f: &PinFunction, pp: (u8, u8)) -> Option<String> {
    let (port, pin) = pp;
    let list = |pins: &[u8]| {
        pins.iter()
            .map(|p| label((2, *p)))
            .collect::<Vec<_>>()
            .join(" or ")
    };
    if let PinFunction::TimerPwm { timer, .. } = f {
        return (port != 1).then(|| format!("PWM{timer} reaches P1 only"));
    }
    let (inst, role) = serial_role(f)?;
    let twi = matches!(role, "scl" | "sda");
    let domain = match (inst, port) {
        (30, 0) | (20..=22, 1) => None,
        (0, 2) => {
            let ok: Vec<u8> = [20, 21].iter().filter_map(|g| nrf54_p2_pin(role, *g)).collect();
            (!ok.contains(&pin)).then(|| format!("SERIAL00 reaches only its dedicated P2 pins - {} for this signal", list(&ok)))
        }
        (20 | 21, 2) if !twi && nrf54_p2_pin(role, inst) == Some(pin) => {
            return Some(format!(
                "a cross-domain pin SERIAL{inst} reaches only in Constant Latency mode\n    // (POWER TASKS_CONSTLAT), which this code does not start"
            ));
        }
        (20 | 21, 2) if !twi => Some(format!(
            "SERIAL{inst} reaches P1, and on P2 only {} for this signal",
            list(&nrf54_p2_pin(role, inst).into_iter().collect::<Vec<_>>())
        )),
        (30, _) => Some("SERIAL30 reaches P0 only".to_owned()),
        (0, _) => Some("SERIAL00 reaches its dedicated P2 pins only".to_owned()),
        _ => Some(format!("SERIAL{inst} reaches P1 only")),
    };
    if domain.is_some() {
        return domain;
    }
    // In the right domain: the clock signal still needs a clock pin. SDA and
    // MOSI/MISO need none - only "close to the clock pin".
    (matches!(role, "sck" | "scl") && !NRF54_CLOCK_PINS.contains(&pp)).then(|| {
        let pins: Vec<String> = NRF54_CLOCK_PINS.iter().map(|p| label(*p)).collect();
        format!(
            "not a clock pin, which SCK and SCL must use - in the QFN48 package\n    // {}",
            pins.join(", ")
        )
    })
}

/// Whether `f` works from pin `pp` on part `c` with nothing else to set up:
/// always on an nRF52 or the nRF5340, and on the nRF54L when
/// [`nrf54_pin_rule`] has nothing to say - so a Constant Latency pin counts
/// as NO. The built-in kits offer only functions this accepts.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn pin_fits(c: &NrfChip, f: &PinFunction, pp: (u8, u8)) -> bool {
    !c.nrf54() || nrf54_pin_rule(f, pp).is_none()
}

/// A comment per signal an nRF54L15 cannot drive from the pin it is wired
/// to: the wrong power domain, a P2 pin that is not dedicated to it, or a
/// clock signal off a clock pin.
///
/// A built-in kit offers no such pairing; a definition made by hand can, and
/// this is the only place that would say so.
fn domain_notes(mcu: &Mcu) -> String {
    let Some(c) = chip(&mcu.family).filter(|c| c.nrf54()) else {
        return String::new();
    };
    let mut o = String::new();
    for p in mcu.iter_all_pins().filter(|p| !p.reserved) {
        let (Some(pp), Some(sig)) = (nrf_pin(&p.name), signal_name(&p.selected_function)) else {
            continue;
        };
        if let Some(why) = nrf54_pin_rule(&p.selected_function, pp) {
            o.push_str(&format!(
                "    // {sig} is on {}, which the {} cannot use for it: {why}.\n",
                label(pp),
                c.part
            ));
        }
    }
    o
}

/// Which pin carries each role of one bus instance.
///
/// Only instances of `kind` the part has: a pad offering SPIM2 on a part
/// without one is reported by `missing_block_notes`, not built.
fn bus_pins(
    mcu: &Mcu,
    kind: &str,
    want: impl Fn(&PinFunction) -> Option<(u8, &'static str)>,
) -> Vec<(u8, &'static str, (u8, u8))> {
    let mut out = Vec::new();
    for p in mcu.iter_all_pins().filter(|p| !p.reserved) {
        let Some(pp) = nrf_pin(&p.name) else {
            continue;
        };
        if let Some((inst, role)) = want(&p.selected_function)
            && block_present(mcu, kind, Some(inst))
        {
            out.push((inst, role, pp));
        }
    }
    // Sorted so that, when two pads claim one role, the lowest pin wins - the
    // same pad `ambiguity_notes` names as the one configured.
    out.sort_unstable();
    out
}

fn role_of(pins: &[(u8, &'static str, (u8, u8))], inst: u8, role: &str) -> Option<(u8, u8)> {
    pins.iter()
        .find(|(i, r, _)| *i == inst && *r == role)
        .map(|(_, _, pp)| *pp)
}

fn instances(pins: &[(u8, &'static str, (u8, u8))]) -> Vec<u8> {
    let mut v: Vec<u8> = pins.iter().map(|(i, _, _)| *i).collect();
    v.dedup();
    v
}

fn uart_pins(mcu: &Mcu) -> Vec<(u8, &'static str, (u8, u8))> {
    bus_pins(mcu, "uarte", |f| match f {
        PinFunction::UsartTx(i) => Some((*i, "txd")),
        PinFunction::UsartRx(i) => Some((*i, "rxd")),
        PinFunction::UsartCts(i) => Some((*i, "cts")),
        PinFunction::UsartRts(i) => Some((*i, "rts")),
        _ => None,
    })
}

fn spi_pins(mcu: &Mcu) -> Vec<(u8, &'static str, (u8, u8))> {
    bus_pins(mcu, "spim", |f| match f {
        PinFunction::SpiSck(i) => Some((*i, "sck")),
        PinFunction::SpiMosi(i) => Some((*i, "mosi")),
        PinFunction::SpiMiso(i) => Some((*i, "miso")),
        _ => None,
    })
}

fn i2c_pins(mcu: &Mcu) -> Vec<(u8, &'static str, (u8, u8))> {
    bus_pins(mcu, "twim", |f| match f {
        PinFunction::I2cSda(i) => Some((*i, "sda")),
        PinFunction::I2cScl(i) => Some((*i, "scl")),
        _ => None,
    })
}

/// The signal a pin carries, as one name: `"UARTE0 TXD"`, `"PWM0 channel 2"`.
///
/// Two pads CAN claim one signal here more easily than anywhere else: every
/// pad offers every instance, so nothing in the definition stops a user from
/// putting UARTE0 TXD on two pads. The HAL takes one.
fn signal_name(f: &PinFunction) -> Option<String> {
    Some(match f {
        PinFunction::UsartTx(i) => format!("UARTE{i} TXD"),
        PinFunction::UsartRx(i) => format!("UARTE{i} RXD"),
        PinFunction::UsartCts(i) => format!("UARTE{i} CTS"),
        PinFunction::UsartRts(i) => format!("UARTE{i} RTS"),
        PinFunction::SpiSck(i) => format!("SPIM{i} SCK"),
        PinFunction::SpiMosi(i) => format!("SPIM{i} MOSI"),
        PinFunction::SpiMiso(i) => format!("SPIM{i} MISO"),
        PinFunction::I2cSda(i) => format!("TWIM{i} SDA"),
        PinFunction::I2cScl(i) => format!("TWIM{i} SCL"),
        PinFunction::TimerPwm { timer, channel } => format!("PWM{timer} channel {channel}"),
        _ => return None,
    })
}

/// Say so when two pads claim one signal; the lowest pin is the one
/// configured, and the note names the rest.
fn ambiguity_notes(mcu: &Mcu) -> String {
    let mut by_signal: std::collections::BTreeMap<String, Vec<(u8, u8)>> =
        std::collections::BTreeMap::new();
    for p in mcu.iter_all_pins().filter(|p| !p.reserved) {
        let (Some(pp), Some(sig)) = (nrf_pin(&p.name), signal_name(&p.selected_function)) else {
            continue;
        };
        by_signal.entry(sig).or_default().push(pp);
    }
    let mut o = String::new();
    for (sig, mut pads) in by_signal {
        if pads.len() < 2 {
            continue;
        }
        pads.sort_unstable();
        let used = label(pads[0]);
        let rest: Vec<String> = pads[1..].iter().map(|pp| label(*pp)).collect();
        o.push_str(&format!(
            "    // {sig} is wired to {used} and {}. Only {used} is configured:\n",
            rest.join(" and ")
        ));
        o.push_str(
            "    // the PSEL register holds one pin. Unassign the other on the Pins canvas.\n",
        );
    }
    o
}

/// `port0.p0_06.into_push_pull_output(Level::High).degrade()` — an output
/// handed to a peripheral, at the level the line idles at until the block
/// takes it over: High for a UART's TXD and RTS, Low for a clock, a data line
/// or a PWM pad (a speaker pad driven High for a moment is a click).
fn out_arg(hal: &str, pp: (u8, u8), level: &str) -> String {
    format!(
        "{}.into_push_pull_output({hal}::gpio::Level::{level}).degrade()",
        field(pp)
    )
}

/// `port1.p1_08.into_floating_input().degrade()` — an input handed to a
/// peripheral.
fn in_arg(pp: (u8, u8)) -> String {
    format!("{}.into_floating_input().degrade()", field(pp))
}

fn opt(arg: Option<String>) -> String {
    arg.map_or("None".to_owned(), |a| format!("Some({a})"))
}

/// UARTE, SPIM and TWIM, in that order.
///
/// A UARTE needs both TXD and RXD (`uarte::Pins` has no `Option` for them);
/// CTS and RTS ride along when wired, and the HAL turns flow control on only
/// when it has BOTH. A SPIM needs SCK and takes MOSI and MISO as options. A
/// TWIM needs both lines.
fn bus_lines(mcu: &Mcu, hal: &str) -> String {
    let mut o = ambiguity_notes(mcu);
    o.push_str(&missing_block_notes(mcu));

    let uart = uart_pins(mcu);
    for i in instances(&uart) {
        let (Some(txd), Some(rxd)) = (role_of(&uart, i, "txd"), role_of(&uart, i, "rxd")) else {
            o.push_str(&format!(
                "    // UARTE{i}: only one of TXD/RXD is wired, and the constructor takes the\n    // pair. Wire the other pad on the Pins canvas.\n"
            ));
            continue;
        };
        let cts = role_of(&uart, i, "cts").map(in_arg);
        let rts = role_of(&uart, i, "rts").map(|pp| out_arg(hal, pp, "High"));
        o.push_str(&format!(
            "    let uarte{i} = pins::configs::uarte{i}::init(\n        p.UARTE{i},\n        {},\n        {},\n        {},\n        {},\n    );\n    let _ = &uarte{i};\n",
            out_arg(hal, txd, "High"),
            in_arg(rxd),
            opt(cts),
            opt(rts),
        ));
    }

    let spi = spi_pins(mcu);
    for i in instances(&spi) {
        let Some(sck) = role_of(&spi, i, "sck") else {
            o.push_str(&format!(
                "    // SPIM{i}: SCK is not wired, and a SPIM without a clock is nothing.\n    // MOSI and MISO are each optional; SCK is not.\n"
            ));
            continue;
        };
        let mosi = role_of(&spi, i, "mosi").map(|pp| out_arg(hal, pp, "Low"));
        let miso = role_of(&spi, i, "miso").map(in_arg);
        o.push_str(&format!(
            "    let spim{i} = pins::configs::spim{i}::init(\n        p.SPIM{i},\n        {},\n        {},\n        {},\n    );\n    let _ = &spim{i};\n",
            out_arg(hal, sck, "Low"),
            opt(mosi),
            opt(miso),
        ));
    }

    let i2c = i2c_pins(mcu);
    for i in instances(&i2c) {
        let (Some(scl), Some(sda)) = (role_of(&i2c, i, "scl"), role_of(&i2c, i, "sda")) else {
            o.push_str(&format!(
                "    // TWIM{i}: SCL and SDA are taken together; wire the missing one.\n"
            ));
            continue;
        };
        o.push_str(&format!(
            "    let twim{i} = pins::configs::twim{i}::init(\n        p.TWIM{i},\n        {},\n        {},\n    );\n    let _ = &twim{i};\n",
            in_arg(scl),
            in_arg(sda),
        ));
    }
    o
}

// ── PWM and SAADC ───────────────────────────────────────────────────────────

/// One block's wired channels: `(channel, pin)`, ascending.
type PwmChannels = Vec<(u8, (u8, u8))>;

/// Every wired PWM channel, by instance, the lowest pin winning a channel two
/// pads claim (see `ambiguity_notes`).
fn pwm_channels(mcu: &Mcu) -> std::collections::BTreeMap<u8, PwmChannels> {
    let mut all: Vec<(u8, u8, (u8, u8))> = mcu
        .iter_all_pins()
        .filter(|p| !p.reserved)
        .filter_map(|p| match p.selected_function {
            PinFunction::TimerPwm { timer, channel } => {
                nrf_pin(&p.name).map(|pp| (timer, channel, pp))
            }
            _ => None,
        })
        .filter(|(timer, _, _)| block_present(mcu, "pwm", Some(*timer)))
        .collect();
    all.sort_unstable();
    let mut by_inst: std::collections::BTreeMap<u8, PwmChannels> =
        std::collections::BTreeMap::new();
    for (inst, ch, pp) in all {
        let chans = by_inst.entry(inst).or_default();
        if chans.iter().all(|(c, _)| *c != ch) {
            chans.push((ch, pp));
        }
    }
    by_inst
}

fn pwm_adc_lines(mcu: &Mcu, hal: &str) -> String {
    let mut o = String::new();

    for (inst, chans) in pwm_channels(mcu) {
        let args: Vec<String> = chans
            .iter()
            .map(|(_, pp)| format!("        {},\n", out_arg(hal, *pp, "Low")))
            .collect();
        o.push_str(&format!(
            "    let pwm{inst} = pins::configs::pwm{inst}::init(\n        p.PWM{inst},\n{}    );\n    let _ = &pwm{inst};\n",
            args.join("")
        ));
    }

    let mut adc: Vec<(u8, (u8, u8), String)> = mcu
        .iter_all_pins()
        .filter(|p| !p.reserved)
        .filter_map(|p| match p.selected_function {
            PinFunction::AdcChannel { channel, .. } => {
                nrf_pin(&p.name).map(|pp| (channel, pp, var_suffix(&p.selected_function)))
            }
            _ => None,
        })
        .collect();
    adc.retain(|a| block_present(mcu, "saadc", Some(a.0)));
    adc.sort_unstable();
    if !adc.is_empty() {
        o.push_str("    // One SAADC for every analog input; a read names the pin it samples.\n");
        o.push_str(&format!(
            "    let mut saadc = {hal}::saadc::Saadc::new(p.SAADC, {hal}::saadc::SaadcConfig::default());\n"
        ));
        o.push_str("    let _ = &mut saadc;\n");
        for (channel, pp, sfx) in &adc {
            let var = format!("{}_{sfx}", ident(*pp));
            o.push_str(&format!(
                "    // AIN{channel} on {}. Read it with `saadc.read_channel(&mut {var})`.\n",
                label(*pp)
            ));
            // NOT degraded: the SAADC channel is a property of the typed pin.
            o.push_str(&format!(
                "    let mut {var} = {}.into_floating_input();\n    let _ = &mut {var};\n",
                field(*pp)
            ));
        }
    }
    o
}

/// USB and QSPI on nrf-hal.
///
/// USB: `Usbd` is a `usb-device` bus, and the allocator is BORROWED by the
/// serial class and the device, so all three are built here in `main`'s
/// scope - the shape the F1 and ESP OTG paths have, for the same reason.
///
/// QSPI: nrf-hal has no QSPI driver at all, so Blocking says so and leaves
/// the pads alone; embassy-nrf has one, on the Async runtime.
fn usb_qspi_lines(mcu: &Mcu, hal: &str) -> String {
    let mut o = String::new();
    if usb_wired(mcu) {
        o.push_str("\n    // ── USB (USBD) ──\n");
        o.push_str(&usb_identity(mcu));
        o.push_str(&format!(
            "    let usb_bus = usb_device::bus::UsbBusAllocator::new({hal}::usbd::Usbd::new(\n        {hal}::usbd::UsbPeripheral::new(p.USBD, &clocks),\n    ));\n"
        ));
        o.push_str(
            "    // These two ARE the device. They must be `mut` for `poll`, and they stay\n    // unused until you write that poll into your loop:\n    //     if usb_dev.poll(&mut [&mut usb_serial]) { /* usb_serial.read / write */ }\n    // `poll` must run often - more than once a millisecond while enumerating.\n",
        );
        o.push_str(&format!(
            "{ALLOW}    let mut usb_serial = usbd_serial::SerialPort::new(&usb_bus);\n{ALLOW}    let mut usb_dev = usb_device::device::UsbDeviceBuilder::new(\n        &usb_bus,\n        usb_device::device::UsbVidPid(USB_VID, USB_PID),\n    )\n    .strings(&[usb_device::device::StringDescriptors::default().product(USB_PRODUCT)])\n    .unwrap()\n    .device_class(usbd_serial::USB_CLASS_CDC)\n    .build();\n"
        ));
    }
    if qspi_touched(mcu) && block_present(mcu, "qspi", None) {
        o.push_str("\n    // QSPI is NOT built: nrf-hal has no QSPI driver. The Async runtime has\n    // one (embassy-nrf's `qspi::Qspi`) and generates it from the same pads.\n");
    }
    o
}

// ── The generated region ────────────────────────────────────────────────────

fn section(mcu: &Mcu) -> String {
    let Some(hal) = hal_crate(&mcu.family) else {
        return async_section(mcu);
    };
    let mut o = String::new();
    o.push_str(GEN_BEGIN);
    o.push('\n');
    o.push_str("#[cortex_m_rt::entry]\n");
    o.push_str("fn main() -> ! {\n");
    o.push_str(&format!(
        "    let p = {hal}::pac::Peripherals::take().unwrap();\n\n"
    ));
    o.push_str(&clock_lines(mcu, &hal));
    // After the clocks, so the LFCLK source the Clock tab picked is the one the
    // WDT will count. Configured only - nothing bites until `activate`.
    o.push_str(&super::watchdog_gen::nrf_init_lines(&mcu.watchdog, false, "WDT"));
    // Port 1 only where the definition names a P1 pin: the nRF52832, 52810 and
    // 52811 have P0 alone, and their HALs have no `p1` module to take.
    let has_p1 = mcu
        .iter_all_pins()
        .any(|p| nrf_pin(&p.name).is_some_and(|(port, _)| port == 1));
    if has_p1 {
        o.push_str("    // Both ports, taken once. Every pad below is moved out of one of them.\n");
    } else {
        o.push_str("    // The port, taken once. Every pad below is moved out of it.\n");
    }
    o.push_str("    #[allow(unused_variables)]\n");
    o.push_str(&format!(
        "    let port0 = {hal}::gpio::p0::Parts::new(p.P0);\n"
    ));
    if has_p1 {
        o.push_str("    #[allow(unused_variables)]\n");
        o.push_str(&format!(
            "    let port1 = {hal}::gpio::p1::Parts::new(p.P1);\n"
        ));
    }
    o.push('\n');
    o.push_str(&nfc_note(mcu));
    let gpio = gpio_lines(mcu, &hal);
    let rest = format!(
        "{}{}{}",
        bus_lines(mcu, &hal),
        pwm_adc_lines(mcu, &hal),
        usb_qspi_lines(mcu, &hal)
    );
    o.push_str(&gpio);
    if !gpio.is_empty() && !rest.is_empty() {
        o.push('\n');
    }
    o.push_str(&rest);
    o.push_str(GEN_END);
    o.push('\n');
    o
}

/// The one header line that names the runtime: which HAL wrote the block below.
fn hal_line(mcu: &Mcu, is_async: bool) -> String {
    if is_async {
        format!("// MCU: {} | HAL: embassy-nrf (async)\n", mcu.name)
    } else {
        let krate = chip(&mcu.family).and_then(|c| c.nrf_hal).unwrap_or("embassy-nrf");
        format!("// MCU: {} | HAL: {krate} (blocking)\n", mcu.name)
    }
}

/// The header both runtimes write, differing only in [`hal_line`].
///
/// A runtime switch keeps it (see [`refresh_hal_line`]), so every import here
/// must resolve on Blocking AND on Async. The `embedded_hal` line does:
/// `Cargo.toml` carries embedded-hal 1.0 beside either nRF HAL line. An
/// `embassy_nrf` import here would not, which is why the Async block imports
/// its names inside `main`.
fn header(mcu: &Mcu, is_async: bool) -> String {
    format!(
        "{AUTOGEN_BANNER}\n\
         {}\
         {}\n\
         #![no_std]\n\
         #![no_main]\n\
         \n\
         pub mod pins;\n\
         \n\
         use panic_halt as _;\n\
         // Where `set_high` / `is_low` / `toggle` come from on Blocking: nrf-hal\n\
         // implements embedded-hal 1.0 and does not re-export it. On Async,\n\
         // embassy-nrf's own methods of those names take precedence.\n\
         #[allow(unused_imports)]\n\
         use embedded_hal::digital::{{InputPin, OutputPin, StatefulOutputPin}};\n\
         \n",
        hal_line(mcu, is_async),
        mcu_id_marker_line(&mcu.id),
    )
}

/// `head` (everything above the markers) with its `// MCU:` line swapped for
/// `wanted`, and every other line kept as the user left it.
///
/// A runtime switch re-splices only the marked block, so without this an
/// Async file would go on saying "(blocking)". The ESP backend refreshes its
/// provenance line for the same reason. A header with no such line is left
/// alone: the user took it out.
fn refresh_hal_line(head: &str, wanted: &str) -> String {
    let mut out = String::with_capacity(head.len() + wanted.len());
    let mut done = false;
    for line in head.split_inclusive('\n') {
        if !done && line.starts_with("// MCU: ") {
            out.push_str(wanted);
            done = true;
        } else {
            out.push_str(line);
        }
    }
    out
}

// ── src/pins/configs/*.rs ───────────────────────────────────────────────────

/// The speed this bus runs at, as the Virtual Module has it.
fn bus_speed(mcu: &Mcu, kind: &str, n: u8) -> u32 {
    use crate::panels::mcu_module::modules;
    match kind {
        "uarte" => modules::usart_configs(&mcu.modules)
            .get(&n)
            .map_or(115_200, |c| c.baud_rate),
        "spim" => modules::spi_configs(&mcu.modules)
            .get(&n)
            .map_or(1_000_000, |c| c.clock_hz),
        _ => modules::i2c_configs(&mcu.modules)
            .get(&n)
            .map_or(400_000, |c| c.clock_hz),
    }
}

/// The UARTE's fixed baud settings, as the PAC names them.
pub(crate) const BAUDS: [(u32, &str); 18] = [
    (1_200, "BAUD1200"),
    (2_400, "BAUD2400"),
    (4_800, "BAUD4800"),
    (9_600, "BAUD9600"),
    (14_400, "BAUD14400"),
    (19_200, "BAUD19200"),
    (28_800, "BAUD28800"),
    (31_250, "BAUD31250"),
    (38_400, "BAUD38400"),
    (56_000, "BAUD56000"),
    (57_600, "BAUD57600"),
    (76_800, "BAUD76800"),
    (115_200, "BAUD115200"),
    (230_400, "BAUD230400"),
    (250_000, "BAUD250000"),
    (460_800, "BAUD460800"),
    (921_600, "BAUD921600"),
    (1_000_000, "BAUD1M"),
];

/// The nearest baud the UARTE has.
pub(crate) fn baud_variant(hz: u32) -> (u32, &'static str) {
    BAUDS
        .iter()
        .copied()
        .min_by_key(|(b, _)| b.abs_diff(hz))
        .unwrap_or((115_200, "BAUD115200"))
}

/// The highest SPIM rate at or below `hz`. SPIM0..2 top out at 8 MHz; the
/// PAC's M16 and M32 belong to SPIM3, which the definition does not offer.
fn spim_frequency(hz: u32) -> (u32, &'static str) {
    const RATES: [(u32, &str); 7] = [
        (125_000, "K125"),
        (250_000, "K250"),
        (500_000, "K500"),
        (1_000_000, "M1"),
        (2_000_000, "M2"),
        (4_000_000, "M4"),
        (8_000_000, "M8"),
    ];
    RATES
        .iter()
        .copied()
        .rev()
        .find(|(r, _)| *r <= hz)
        .unwrap_or(RATES[0])
}

/// The highest TWIM rate at or below `hz`.
fn twim_frequency(hz: u32) -> (u32, &'static str) {
    const RATES: [(u32, &str); 3] = [(100_000, "K100"), (250_000, "K250"), (400_000, "K400")];
    RATES
        .iter()
        .copied()
        .rev()
        .find(|(r, _)| *r <= hz)
        .unwrap_or(RATES[0])
}

fn bus_config_file(
    hal: &str,
    kind: &str,
    n: u8,
    hz: u32,
    frame: Option<&UsartModuleConfig>,
    spi_mode: u8,
    // The TWIM bus's device modules (`common::i2c_device_mods`), empty for the
    // other two kinds. Resolved by the caller the way `hz` and `spi_mode` are,
    // so this stays a pure formatter.
    i2c_mods: &str,
) -> String {
    use crate::panels::mcu_module::modules::{Parity, StopBits};
    let mut o = String::new();
    o.push_str("// <<< GENERATED>>>\n");
    o.push_str(
        "// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.\n",
    );
    match kind {
        "uarte" => {
            let (got, variant) = baud_variant(hz);
            if got != hz {
                o.push_str(&format!(
                    "// {hz} baud asked for; the UARTE has fixed rates and {got} is the nearest.\n"
                ));
            }
            o.push_str(&format!(
                "pub const BAUDRATE: {hal}::uarte::Baudrate = {hal}::uarte::Baudrate::{variant};\n"
            ));
            let d = UsartModuleConfig::new(0);
            let c = frame.unwrap_or(&d);
            let parity = match c.parity {
                Parity::None => "EXCLUDED",
                Parity::Even => "INCLUDED",
                Parity::Odd => {
                    o.push_str("// Odd parity asked for; the UARTE generates even parity only.\n");
                    "INCLUDED"
                }
            };
            o.push_str(&format!(
                "pub const PARITY: {hal}::uarte::Parity = {hal}::uarte::Parity::{parity};\n"
            ));
            if c.data_bits != 8 || c.stop_bits != StopBits::One {
                o.push_str("// The UARTE frames 8 data bits and 1 stop bit; the module's other\n// setting is not reachable through the HAL.\n");
            }
        }
        "spim" => {
            let (got, variant) = spim_frequency(hz);
            if got != hz {
                o.push_str(&format!(
                    "// {hz} Hz asked for; the SPIM has fixed rates and {got} is the highest at or below it.\n"
                ));
            }
            o.push_str(&format!(
                "pub const FREQUENCY: {hal}::spim::Frequency = {hal}::spim::Frequency::{variant};\n"
            ));
            // In the regenerated half, because the Virtual Module owns it. It
            // sat below the markers as a fixed `MODE_0` at first, so the
            // panel's mode combo changed nothing on this chip.
            o.push_str(&format!(
                "/// Mode {mode}, as the Virtual Module has it.\npub const MODE: {hal}::spim::Mode = {hal}::spim::MODE_{mode};\n",
                mode = spi_mode.min(3)
            ));
        }
        _ => {
            let (got, variant) = twim_frequency(hz);
            if got != hz {
                o.push_str(&format!(
                    "// {hz} Hz asked for; the TWIM has fixed rates and {got} is the highest at or below it.\n"
                ));
            }
            o.push_str(&format!(
                "pub const FREQUENCY: {hal}::twim::Frequency = {hal}::twim::Frequency::{variant};\n"
            ));
            o.push_str(i2c_mods);
        }
    }
    o.push_str("// <<< GENERATED END >>>\n\n");
    o.push_str("// Everything below is editable — your changes are preserved on regeneration.\n");
    // The TWIM takes two inputs and nothing else, and an unused import is a
    // warning the matrix counts as a failure.
    if kind == "twim" {
        o.push_str(&format!("use {hal}::gpio::{{Floating, Input, Pin}};\n\n"));
    } else {
        o.push_str(&format!(
            "use {hal}::gpio::{{Floating, Input, Output, Pin, PushPull}};\n\n"
        ));
    }
    match kind {
        "uarte" => {
            o.push_str(&format!(
                "/// The concrete type `init` hands back, so it can be a struct field.\npub type Handle = {hal}::uarte::Uarte<{hal}::pac::UARTE{n}>;\n\n"
            ));
            o.push_str(&format!(
                "/// UARTE{n} at BAUDRATE. Any pin can carry any role (the PSEL registers\n/// hold pin numbers), so `main.rs` picks them. Flow control is on only when\n/// BOTH `cts` and `rts` are given: the HAL sets HWFC from the pair.\npub fn init(\n    uarte: {hal}::pac::UARTE{n},\n    txd: Pin<Output<PushPull>>,\n    rxd: Pin<Input<Floating>>,\n    cts: Option<Pin<Input<Floating>>>,\n    rts: Option<Pin<Output<PushPull>>>,\n) -> Handle {{\n    {hal}::uarte::Uarte::new(\n        uarte,\n        {hal}::uarte::Pins {{ rxd, txd, cts, rts }},\n        PARITY,\n        BAUDRATE,\n    )\n}}\n"
            ));
        }
        "spim" => {
            o.push_str(&format!(
                "/// The concrete type `init` hands back.\npub type Handle = {hal}::spim::Spim<{hal}::pac::SPIM{n}>;\n\n"
            ));
            o.push_str(
                "/// The byte clocked out while only receiving.\npub const ORC: u8 = 0x00;\n\n",
            );
            o.push_str(&format!(
                "/// SPIM{n} at FREQUENCY. SCK is required; MOSI and MISO are each optional,\n/// which is how a write-only or read-only bus is built. Chip select is a\n/// plain GPIO output, driven by the caller.\npub fn init(\n    spim: {hal}::pac::SPIM{n},\n    sck: Pin<Output<PushPull>>,\n    mosi: Option<Pin<Output<PushPull>>>,\n    miso: Option<Pin<Input<Floating>>>,\n) -> Handle {{\n    {hal}::spim::Spim::new(\n        spim,\n        {hal}::spim::Pins {{\n            sck: Some(sck),\n            mosi,\n            miso,\n        }},\n        FREQUENCY,\n        MODE,\n        ORC,\n    )\n}}\n"
            ));
        }
        _ => {
            o.push_str(&format!(
                "/// The concrete type `init` hands back.\npub type Handle = {hal}::twim::Twim<{hal}::pac::TWIM{n}>;\n\n"
            ));
            o.push_str(&format!(
                "/// TWIM{n} at FREQUENCY. Both lines are inputs to the GPIO block: the\n/// TWIM drives them open-drain itself, and the pull-ups are on the bus.\npub fn init(\n    twim: {hal}::pac::TWIM{n},\n    scl: Pin<Input<Floating>>,\n    sda: Pin<Input<Floating>>,\n) -> Handle {{\n    {hal}::twim::Twim::new(twim, {hal}::twim::Pins {{ scl, sda }}, FREQUENCY)\n}}\n"
            ));
        }
    }
    o
}

/// The PWM counter's base clock, before the prescaler.
const PWM_CLOCK_HZ: u32 = 16_000_000;
/// The highest COUNTERTOP the block takes.
const PWM_MAX_TOP: u32 = 32_767;

/// `(divider, Prescaler name)` for a PWM asked to run at `freq_hz`: the
/// smallest divider whose counter top fits in 15 bits, so the duty keeps as
/// much resolution as the frequency allows.
fn pwm_prescaler(freq_hz: u32) -> (u32, &'static str) {
    const DIVS: [(u32, &str); 8] = [
        (1, "Div1"),
        (2, "Div2"),
        (4, "Div4"),
        (8, "Div8"),
        (16, "Div16"),
        (32, "Div32"),
        (64, "Div64"),
        (128, "Div128"),
    ];
    let freq = freq_hz.max(1);
    DIVS.iter()
        .copied()
        .find(|(d, _)| PWM_CLOCK_HZ / d / freq <= PWM_MAX_TOP)
        .unwrap_or(DIVS[7])
}

/// The frequency the pad really sees for `freq_hz` through `div`: the HAL's
/// `set_period` computes `top = clock / div / freq` in integers, and the
/// output is `clock / div / top`, clamped where `top` would overflow.
fn pwm_actual_hz(freq_hz: u32, div: u32) -> u32 {
    let clk = PWM_CLOCK_HZ / div;
    let top = (clk / freq_hz.max(1)).clamp(1, PWM_MAX_TOP);
    clk / top
}

/// `src/pins/configs/pwm{n}.rs` — one PWM block, its frequency and the duty
/// of each channel it drives.
fn pwm_config_file(mcu: &Mcu, inst: u8, chans: &[(u8, (u8, u8))]) -> String {
    // Only reached through `NrfBackend::config_files`, which writes no PWM
    // file for a part without an nrf-hal crate - and that part has no PWM.
    let hal = hal_crate(&mcu.family).unwrap_or_default();
    let cfg = crate::panels::mcu_module::modules::timer_configs(&mcu.modules);
    let cfg = cfg.get(&inst);
    let want = cfg.map_or(0, |c| c.freq_hz);
    let mut o = String::new();

    o.push_str("// <<< GENERATED>>>\n");
    o.push_str(
        "// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.\n",
    );
    if want > 0 {
        let (div, name) = pwm_prescaler(want);
        let got = pwm_actual_hz(want, div);
        o.push_str(&format!("pub const FREQ_HZ: u32 = {want};"));
        if got != want {
            o.push_str(&format!(" // the pad sees {got} Hz"));
        }
        o.push('\n');
        o.push_str(&format!(
            "/// {} MHz / {div}: the smallest divider that keeps the counter top within 15 bits.\npub const PRESCALER: {hal}::pwm::Prescaler = {hal}::pwm::Prescaler::{name};\n",
            PWM_CLOCK_HZ / 1_000_000
        ));
    } else {
        o.push_str("// No frequency set in the module: the HAL's default stands (16 MHz over a\n// 32767 top, about 488 Hz). Set one in the Virtual Module.\n");
        o.push_str("pub const FREQ_HZ: u32 = 0;\n");
        o.push_str(&format!(
            "pub const PRESCALER: {hal}::pwm::Prescaler = {hal}::pwm::Prescaler::Div1;\n"
        ));
    }
    o.push_str("// Duty per channel, in HUNDREDTHS of a percent — 750 is 7.5 %, which is what a\n");
    o.push_str("// hobby servo wants and what whole percent cannot say.\n");
    for (ch, _) in chans {
        let x100 = cfg.map_or(0, |c| c.duty_x100_of(*ch));
        o.push_str(&format!(
            "pub const DUTY_C{ch}_X100: u32 = {x100}; // {} %\n",
            super::common::duty_percent_str(x100)
        ));
    }
    o.push_str("// <<< GENERATED END >>>\n\n");

    o.push_str("// Everything below is editable — your changes are preserved on regeneration.\n");
    o.push_str(&format!(
        "use {hal}::gpio::{{Output, Pin, PushPull}};\nuse {hal}::pwm::Channel;\n\n"
    ));
    o.push_str(&format!(
        "/// The concrete type `init` hands back. The block owns its four channels;\n/// each wired one is routed to a pad below.\npub type Handle = {hal}::pwm::Pwm<{hal}::pac::PWM{inst}>;\n\n"
    ));

    let params: Vec<String> = chans
        .iter()
        .map(|(ch, pp)| {
            format!(
                "    // channel {ch}, on {}\n    c{ch}: Pin<Output<PushPull>>,\n",
                label(*pp)
            )
        })
        .collect();
    o.push_str(&format!(
        "/// Bring the block up at FREQ_HZ and route each wired channel to its pad.\npub fn init(\n    pwm: {hal}::pac::PWM{inst},\n{}) -> Handle {{\n    let pwm = {hal}::pwm::Pwm::new(pwm);\n",
        params.join("")
    ));
    if want > 0 {
        o.push_str("    // The prescaler first: `set_period` derives the counter top from it.\n");
        o.push_str("    pwm.set_prescaler(PRESCALER);\n");
        o.push_str(&format!(
            "    pwm.set_period({hal}::time::Hertz(FREQ_HZ));\n"
        ));
    }
    for (ch, _) in chans {
        o.push_str(&format!("    pwm.set_output_pin(Channel::C{ch}, c{ch});\n"));
    }
    o.push_str("    pwm.enable();\n");
    o.push_str("    let max = pwm.max_duty() as u32;\n");
    for (ch, _) in chans {
        o.push_str(&format!(
            "    pwm.set_duty_on(Channel::C{ch}, (max * DUTY_C{ch}_X100 / 10_000) as u16);\n"
        ));
    }
    o.push_str("    pwm\n}\n\n");

    let first = chans.first().map_or(0, |(c, _)| *c);
    o.push_str("/// Set a channel's duty in the same units the `DUTY_*` constants above use —\n");
    o.push_str("/// HUNDREDTHS of a percent, so `10_000` is 100 % and `750` is 7.5 %.\n///\n");
    o.push_str("/// A trait rather than an inherent method because `Handle` is nrf-hal's own\n");
    o.push_str("/// type, which this crate does not own. One method per WIRED channel: the\n");
    o.push_str("/// channel is part of the NAME rather than an argument, so a channel this\n");
    o.push_str("/// block has no pad for cannot be asked for at all.\n");
    o.push_str("pub trait DutyHandle {\n");
    o.push_str(&format!(
        "    /// Channel {first}, the first one wired to this block.\n    fn set_duty_pwm_{inst}(&mut self, value: u32);\n"
    ));
    for (ch, _) in chans {
        o.push_str(&format!(
            "\n    /// Channel {ch}.\n    fn set_duty_pwm_{inst}_c{ch}(&mut self, value: u32);\n"
        ));
    }
    o.push_str("}\n\nimpl DutyHandle for Handle {\n");
    o.push_str(&format!(
        "    fn set_duty_pwm_{inst}(&mut self, value: u32) {{\n        self.set_duty_pwm_{inst}_c{first}(value);\n    }}\n"
    ));
    for (ch, _) in chans {
        o.push_str(&format!(
            "\n    fn set_duty_pwm_{inst}_c{ch}(&mut self, value: u32) {{\n        let max = self.max_duty() as u32;\n        self.set_duty_on(Channel::C{ch}, (max * value / 10_000) as u16);\n    }}\n"
        ));
    }
    o.push_str("}\n");
    o
}

impl FamilyBackend for NrfBackend {
    fn family_id(&self) -> &'static str {
        "nrf52833"
    }

    fn handles(&self, family: &str) -> bool {
        is_nrf(family)
    }

    // `gpio_modes` is the trait default: nrf-hal has all three input pulls and
    // both output drives as `into_*` methods, which is exactly the full set.

    fn config_files(&self, mcu: &Mcu) -> Vec<(String, String)> {
        // On embassy-nrf every bus is built inline in main.rs, as on Async:
        // the watchdog's is the only file.
        let Some(hal) = hal_crate(&mcu.family) else {
            return super::watchdog_gen::nrf_config_files(&mcu.watchdog, &mcu.family, false);
        };
        let mut out: Vec<(String, String)> = pwm_channels(mcu)
            .into_iter()
            .map(|(inst, chans)| (format!("pwm{inst}.rs"), pwm_config_file(mcu, inst, &chans)))
            .collect();

        let ucfgs = crate::panels::mcu_module::modules::usart_configs(&mcu.modules);
        let scfgs = crate::panels::mcu_module::modules::spi_configs(&mcu.modules);
        // The I2C modules, so each twim file can carry its own device address.
        let icfgs = crate::panels::mcu_module::modules::i2c_configs(&mcu.modules);
        // Only a bus `main.rs` can construct gets a file - the same rule
        // `bus_lines` applies, so a file never exists without its `init` call.
        for (kind, required, pins) in [
            ("uarte", &["txd", "rxd"][..], uart_pins(mcu)),
            ("spim", &["sck"][..], spi_pins(mcu)),
            ("twim", &["scl", "sda"][..], i2c_pins(mcu)),
        ] {
            for i in instances(&pins) {
                if required.iter().all(|r| role_of(&pins, i, r).is_some()) {
                    let frame = (kind == "uarte").then(|| ucfgs.get(&i)).flatten();
                    let spi_mode = scfgs.get(&i).map_or(0, |c| c.mode);
                    // Only the TWIM bus is a folder - its devices each get a
                    // file beside its `mod.rs`.
                    let mods = if kind == "twim" {
                        super::common::i2c_device_mods(icfgs.get(&i))
                    } else {
                        String::new()
                    };
                    let body = bus_config_file(
                        &hal,
                        kind,
                        i,
                        bus_speed(mcu, kind, i),
                        frame,
                        spi_mode,
                        &mods,
                    );
                    if kind == "twim" {
                        out.extend(super::common::i2c_bus_files(
                            &format!("twim{i}"),
                            body,
                            icfgs.get(&i),
                        ));
                    } else {
                        out.push((format!("{kind}{i}.rs"), body));
                    }
                }
            }
        }
        // Into this list, not beside it: an empty list drops the whole
        // `configs/` subtree, so a watchdog alone has to count like any bus.
        out.extend(super::watchdog_gen::nrf_config_files(
            &mcu.watchdog,
            &mcu.family,
            false,
        ));
        out
    }

    fn fresh_main_rs(&self, mcu: &Mcu) -> String {
        format!("{}{}{USER_TAIL}", header(mcu, false), section(mcu))
    }

    /// Replace ONLY the marked block, keeping what the user wrote on either
    /// side of it. The same splice as the RP backend, for the same reason:
    /// `embassy_async::splice_section` rewrites the header too. The one header
    /// line that names the runtime is refreshed.
    fn update_main_rs(&self, mcu: &Mcu, existing: &str) -> String {
        let (Some(begin), Some(end_start)) = (existing.find(GEN_BEGIN), existing.find(GEN_END))
        else {
            return self.fresh_main_rs(mcu);
        };
        let end = end_start + GEN_END.len();
        format!(
            "{}{}{}",
            refresh_hal_line(&existing[..begin], &hal_line(mcu, false)),
            section(mcu).trim_end_matches('\n'),
            retarget_pristine_tail(&existing[end..], false)
        )
    }
}

// ── Async: embassy-nrf ──────────────────────────────────────────────────────
//
// The same board on embassy-nrf 0.11, a different HAL crate rather than a
// feature of nrf-hal. Every call below was read from the embassy-nrf 0.11.0
// source; `emit_nrf_async_project` is what proves the reading. Against the
// blocking backend:
//
// - `embassy_nrf::init(config)` starts both clocks itself, so the Clock tab
//   becomes two fields of `Config` rather than a `Clocks` chain.
// - Each pin is a field of the `Peripherals` that `init` returns (`p.P0_21`),
//   handed over by value: no ports to split, nothing to degrade.
// - The serial blocks are async and each needs its interrupt bound, so the
//   buses are built in `main.rs` against one `bind_interrupts!` struct, and no
//   bus config files are written - the watchdog's is the only one.
// - An armed input becomes a task that waits on it.

pub struct AsyncNrfBackend;

/// `P0_21` — the `Peripherals` field that owns a pin.
fn periph((port, pin): (u8, u8)) -> String {
    format!("P{port}_{pin:02}")
}

/// The `Peripherals` field and the interrupt vector a serial block is built
/// from, as `(field, vector)`.
///
/// embassy-nrf names a SHARED block after what it shares: SPIM0 and TWIM0 are
/// one peripheral, `TWISPI0`, on one vector. SPIM2 is `SPI2`, and SPIM3 is
/// `SPI3` on a vector named `SPIM3`. The 52820, 52832 and 52840 spell them
/// the same. The small parts do not: on the 52805 and 52810 SPIM0 and TWIM0
/// are SEPARATE blocks (`SPI0`, `TWI0`), and on the 52811 the shared one is
/// SPIM1 with TWIM0, named `TWI0_SPI1`.
pub(super) fn serial_block(family: &str, kind: &str, n: u8) -> (String, String) {
    let same = |s: &str| (s.to_owned(), s.to_owned());
    match (family, kind, n) {
        // UARTE, SPIM and TWIM n are all SERIALn; SPIM4 shares nothing.
        ("nrf5340", "spim", 4) => same("SPIM4"),
        // The nRF54L numbers by power domain: instance 0 is SERIAL00, the
        // others SERIAL20/21/22/30 - the same number the instance has.
        ("nrf54l15", _, 0) => same("SERIAL00"),
        ("nrf54l15", _, _) => same(&format!("SERIAL{n}")),
        ("nrf5340", _, _) => same(&format!("SERIAL{n}")),
        (_, "uarte", _) => same(&format!("UARTE{n}")),
        ("nrf52805" | "nrf52810" | "nrf52811", "spim", 0) => same("SPI0"),
        ("nrf52805" | "nrf52810", "twim", 0) => same("TWI0"),
        ("nrf52811", "spim", 1) | ("nrf52811", "twim", 0) => same("TWI0_SPI1"),
        (_, "spim", 2) => same("SPI2"),
        (_, "spim", 3) => ("SPI3".to_owned(), "SPIM3".to_owned()),
        _ => same(&format!("TWISPI{n}")),
    }
}

/// The UARTE baud as embassy-nrf's PAC spells it: `BAUD115200` becomes
/// `Baud115200`, and `BAUD1M` becomes `Baud1m`.
fn embassy_baud(variant: &str) -> String {
    format!(
        "Baud{}",
        variant.trim_start_matches("BAUD").to_ascii_lowercase()
    )
}

/// `(divider, Prescaler name, counter top, the frequency the pad sees)` for a
/// PWM block asked to run at `freq_hz`.
///
/// Center-aligned counting runs up to the top and back down, so one period
/// spends two tops: it needs half the top for the same frequency.
fn pwm_timing(freq_hz: u32, center: bool) -> (u32, &'static str, u32, u32) {
    let tops_per_period = if center { 2 } else { 1 };
    let counts = freq_hz.max(1).saturating_mul(tops_per_period);
    let (div, name) = pwm_prescaler(counts);
    let clk = PWM_CLOCK_HZ / div;
    let top = (clk / counts).clamp(1, PWM_MAX_TOP);
    (div, name, top, clk / top / tops_per_period)
}

/// GPIO on the async runtime, as `(top-level tasks, main body)`.
///
/// An ARMED input is not a binding: it becomes a task that owns the pin and
/// awaits the edge, the same shape the RP and STM32 async backends use.
/// `Input::wait_for_*` works through the GPIO PORT event and a per-pin SENSE
/// setting, not a GPIOTE channel, so any number of pins can wait at once and
/// the eight GPIOTE channels stay free.
fn async_gpio_lines(mcu: &Mcu) -> (String, String) {
    use crate::panels::mcu_module::pins::logic::pin::model::Edge;
    let mut tasks = String::new();
    let mut pins_out: Vec<String> = Vec::new();
    for p in mcu.iter_all_pins().filter(|p| !p.reserved) {
        let Some(pp) = nrf_pin(&p.name) else {
            continue;
        };
        let var = async_binding(pp, p);
        let what = describe(pp, p);
        match p.selected_function {
            PinFunction::GpioOutput => {
                // Open-drain is a DRIVE here, not a type: `Standard0Disconnect1`
                // pulls low and lets go high, so it starts released.
                let (level, drive) = match GpioMode::for_output(p.io_mode) {
                    GpioMode::OpenDrain => ("High", "Standard0Disconnect1"),
                    _ => ("Low", "Standard"),
                };
                pins_out.push(format!(
                    "    // {what}\n{ALLOW}    let mut {var} = Output::new(p.{}, Level::{level}, OutputDrive::{drive});\n",
                    periph(pp)
                ));
            }
            PinFunction::GpioInput => {
                let pull = match GpioMode::for_input(p.io_mode) {
                    GpioMode::PullUp => "Up",
                    GpioMode::PullDown => "Down",
                    _ => "None",
                };
                let ctor = format!("Input::new(p.{}, Pull::{pull})", periph(pp));
                // On Blocking (the part nrf-hal has no crate for) there is no
                // executor to run the waiting task: the pin is an input, and
                // the edge is said rather than dropped, as on nrf-hal.
                let edge = p.irq.filter(|_| mcu.is_async());
                let Some(edge) = edge else {
                    let mut entry = format!("    // {what}\n{ALLOW}    let {var} = {ctor};\n");
                    if p.irq.is_some() {
                        entry.push_str(&format!(
                            "    // {} is armed for an interrupt, which this Blocking project does\n    // not generate: poll it, or switch to the Async runtime.\n",
                            label(pp)
                        ));
                    }
                    pins_out.push(entry);
                    continue;
                };
                let (wait, desc) = match edge {
                    Edge::Rising => ("wait_for_rising_edge", "A rising edge"),
                    Edge::Falling => ("wait_for_falling_edge", "A falling edge"),
                    Edge::Both => ("wait_for_any_edge", "Either edge"),
                };
                // The body is NOT here: this task is rebuilt on every
                // regeneration, so it calls a hook seeded once below the tail
                // (`common::ensure_edge_hooks`), which is the user's to fill.
                let hook = edge_hook_name(&var);
                tasks.push_str(&format!(
                    "/// {desc} on {what}. The task owns the pin;\n/// `{hook}` below `main` is yours.\n#[embassy_executor::task]\nasync fn {var}_irq(mut pin: embassy_nrf::gpio::Input<'static>) {{\n    loop {{\n        pin.{wait}().await;\n        {hook}(pin.is_high()).await;\n    }}\n}}\n\n"
                ));
                // embassy-executor 0.10: the task FUNCTION returns the Result
                // (its pool can be exhausted), so the `unwrap` sits inside
                // `spawn`, the same as on the RP.
                pins_out.push(format!(
                    "    // {what}\n    let {var} = {ctor};\n    spawner.spawn({var}_irq({var}).unwrap());\n"
                ));
            }
            _ => {}
        }
    }
    (tasks, blank_separated(pins_out))
}

/// The binding an async GPIO line declares: `p0_14_in`, `p0_21_out`. The
/// pin's label is NOT in it, so a rename never renames the task or its hook.
fn async_binding(pp: (u8, u8), p: &Pin) -> String {
    format!("{}_{}", ident(pp), var_suffix(&p.selected_function))
}

/// The pad as the generated comment names it: `P0.14 (pad 5, BTN_A)`.
fn describe(pp: (u8, u8), p: &Pin) -> String {
    format!(
        "{}{}",
        label(pp),
        board_name(&p.name).map_or(String::new(), |b| format!(" ({b})"))
    )
}

/// The hooks the armed inputs' tasks call, in the order the tasks are
/// emitted. Same binding and description as the task, from the same helpers,
/// so the call and the seed cannot drift apart.
fn async_edge_hooks(mcu: &Mcu) -> Vec<EdgeHook> {
    mcu.iter_all_pins()
        .filter(|p| !p.reserved && p.selected_function == PinFunction::GpioInput && p.irq.is_some())
        .filter_map(|p| nrf_pin(&p.name).map(|pp| (pp, p)))
        .map(|(pp, p)| {
            let var = async_binding(pp, p);
            EdgeHook {
                name: edge_hook_name(&var),
                what: describe(pp, p),
                caller: format!("{var}_irq"),
                is_async: true,
            }
        })
        .collect()
}

/// The two clock muxes, as `init`'s `Config`, and the `init` call itself.
fn async_clock_lines(mcu: &Mcu) -> String {
    let c = clock_choice(mcu);
    // USB runs only from the crystal: wired, it overrides the Clock tab.
    let usb_forces_xtal = !c.hfxo && usb_wired(mcu) && mcu.is_async();
    let hf = if c.hfxo || usb_forces_xtal {
        "ExternalXtal"
    } else {
        "Internal"
    };
    let lf = match c.lf {
        LfSource::Rc => "InternalRC",
        LfSource::Synth => "Synthesized",
        LfSource::Xtal => "ExternalXtal",
    };
    let mut o = String::new();
    o.push_str("    // From the Clock tab. HFCLK is 64 MHz either way; the choice is whether\n");
    o.push_str("    // the 32 MHz crystal is started, which the radio and USB need. LFCLK is\n");
    if mcu.is_async() && chip(&mcu.family).is_some_and(|c| c.nrf54()) {
        o.push_str("    // 32.768 kHz, and it clocks the GRTC, which is embassy-time's driver.\n");
    } else if mcu.is_async() {
        o.push_str("    // 32.768 kHz, and it clocks RTC1, which is embassy-time's driver.\n");
    } else {
        o.push_str("    // 32.768 kHz, and it clocks the RTCs and the watchdog.\n");
    }
    if usb_forces_xtal {
        o.push_str("    // The USB module needs the crystal: the Clock tab's internal choice is\n    // overridden, since USB cannot run from the RC oscillator.\n");
    }
    o.push_str("    let mut config = embassy_nrf::config::Config::default();\n");
    o.push_str(&format!(
        "    config.hfclk_source = embassy_nrf::config::HfclkSource::{hf};\n"
    ));
    o.push_str(&format!(
        "    config.lfclk_source = embassy_nrf::config::LfclkSource::{lf};\n"
    ));
    o.push_str("    #[allow(unused_variables)]\n");
    o.push_str("    let p = embassy_nrf::init(config);\n\n");
    o
}

/// The NFC pads on Async: nothing to do by hand, and worth saying why.
fn async_nfc_note(mcu: &Mcu) -> String {
    let used = nfc_pads_used(mcu);
    if used.is_empty() {
        return String::new();
    }
    // The nRF54L has no UICR.NFCPINS: the pads are a register of NFCT itself.
    let how = if chip(&mcu.family).is_some_and(|c| c.nrf54()) {
        "has `init` turn the NFCT pads off\n    // (NFCT.PADCONFIG) on every boot, so they are GPIO from the first one.\n"
    } else {
        "has `init` clear UICR.NFCPINS and reset\n    // once, so they are GPIO from the second boot on, until the UICR is erased.\n"
    };
    format!(
        "    // {} {} the NFC antenna pins. The `nfc-pins-as-gpio` feature on the\n    // embassy-nrf line in Cargo.toml {how}",
        used.join(" and "),
        if used.len() == 1 { "is one of" } else { "are" }
    )
}

/// What the async bus pass produced.
struct AsyncBuses {
    /// `bind_interrupts!` entries, one per vector.
    irqs: Vec<String>,
    /// The lines inside `main`.
    body: String,
    /// Items that must sit at MODULE scope, above the entry point - today the
    /// I2C device-address consts.
    ///
    /// Not folded into `body`: that goes inside `async fn main`, and a const
    /// declared in a function body is private to it, which rustc then reports
    /// as dead code until the user's own code happens to name it. And not
    /// rebuilt by a second pass over the pins either, because which buses get
    /// built here is not a property of the pins alone - `clash` drops a TWIM
    /// whose block a SPIM already took - so the only place that knows is this
    /// loop.
    items: String,
    /// Whether `body` names `static_cell` (a TWIM's RAM buffer, the USB
    /// stack's buffers).
    static_cell: bool,
    /// Whether `body` spawns a task (the USB device's), so `main` needs its
    /// spawner by name.
    spawns: bool,
}

/// UARTE, SPIM, TWIM, PWM and SAADC on embassy-nrf, in that order.
///
/// Each block's peripheral is recorded as it is taken, because SPIMn and TWIMn
/// (n < 2) are ONE block: a second constructor on it would not compile, so the
/// second of the two gets a comment instead.
fn async_bus_lines(mcu: &Mcu) -> AsyncBuses {
    use crate::panels::mcu_module::modules::{
        self, Parity, PwmCounting, PwmMode, PwmOutput, PwmPolarity, SpiBitOrder, StopBits,
    };
    let mut irqs: Vec<String> = Vec::new();
    let mut o = ambiguity_notes(mcu);
    o.push_str(&missing_block_notes(mcu));
    o.push_str(&domain_notes(mcu));
    let mut items = String::new();
    let mut static_cell = false;
    let mut taken: Vec<(String, String)> = Vec::new();
    let clash = |taken: &[(String, String)], peri: &str, me: &str| {
        taken.iter().find(|(p, _)| p == peri).map(|(_, who)| {
            format!(
                "    // {me} is not built: it is the same block as {who} ({peri}), which has it.\n"
            )
        })
    };

    let uart = uart_pins(mcu);
    let ucfgs = modules::usart_configs(&mcu.modules);
    for i in instances(&uart) {
        let (Some(txd), Some(rxd)) = (role_of(&uart, i, "txd"), role_of(&uart, i, "rxd")) else {
            o.push_str(&format!(
                "    // UARTE{i}: only one of TXD/RXD is wired, and the constructor takes the\n    // pair. Wire the other pad on the Pins canvas.\n"
            ));
            continue;
        };
        let (peri, irq) = serial_block(&mcu.family, "uarte", i);
        let var = format!("uarte{i}");
        let hz = bus_speed(mcu, "uarte", i);
        let (got, variant) = baud_variant(hz);
        if got != hz {
            o.push_str(&format!(
                "    // {hz} baud asked for; the UARTE has fixed rates and {got} is the nearest.\n"
            ));
        }
        o.push_str(&format!(
            "    let mut {var}_cfg = embassy_nrf::uarte::Config::default();\n    {var}_cfg.baudrate = embassy_nrf::uarte::Baudrate::{};\n",
            embassy_baud(variant)
        ));
        let d = UsartModuleConfig::new(0);
        let c = ucfgs.get(&i).unwrap_or(&d);
        let parity = match c.parity {
            Parity::None => "Excluded",
            Parity::Even => "Included",
            Parity::Odd => {
                o.push_str("    // Odd parity asked for; the UARTE generates even parity only.\n");
                "Included"
            }
        };
        o.push_str(&format!(
            "    {var}_cfg.parity = embassy_nrf::uarte::Parity::{parity};\n"
        ));
        if c.data_bits != 8 || c.stop_bits != StopBits::One {
            o.push_str("    // The UARTE frames 8 data bits and 1 stop bit; the module's other\n    // setting is not reachable through the HAL.\n");
        }
        let ctor = match (role_of(&uart, i, "cts"), role_of(&uart, i, "rts")) {
            (Some(cts), Some(rts)) => format!(
                "new_with_rtscts(\n        p.{peri},\n        p.{}, // RXD\n        p.{}, // TXD\n        p.{}, // CTS\n        p.{}, // RTS\n        Irqs,\n        {var}_cfg,\n    )",
                periph(rxd),
                periph(txd),
                periph(cts),
                periph(rts)
            ),
            (cts, rts) => {
                if cts.is_some() || rts.is_some() {
                    o.push_str(&format!(
                        "    // UARTE{i}: only {} is wired. embassy-nrf takes flow control as the\n    // CTS+RTS pair, so this port is built without it.\n",
                        if cts.is_some() { "CTS" } else { "RTS" }
                    ));
                }
                format!(
                    "new(\n        p.{peri},\n        p.{}, // RXD\n        p.{}, // TXD\n        Irqs,\n        {var}_cfg,\n    )",
                    periph(rxd),
                    periph(txd)
                )
            }
        };
        o.push_str(&format!(
            "{ALLOW}    let mut {var} = embassy_nrf::uarte::Uarte::{ctor};\n"
        ));
        irqs.push(format!(
            "    {irq} => embassy_nrf::uarte::InterruptHandler<embassy_nrf::peripherals::{peri}>;"
        ));
        taken.push((peri, format!("UARTE{i}")));
    }

    let spi = spi_pins(mcu);
    let scfgs = modules::spi_configs(&mcu.modules);
    for i in instances(&spi) {
        let Some(sck) = role_of(&spi, i, "sck") else {
            o.push_str(&format!(
                "    // SPIM{i}: SCK is not wired, and a SPIM without a clock is nothing.\n    // MOSI and MISO are each optional; SCK is not.\n"
            ));
            continue;
        };
        let (peri, irq) = serial_block(&mcu.family, "spim", i);
        let var = format!("spim{i}");
        // embassy-nrf has a constructor for both data lines and one for each
        // alone, and none for a clock with neither.
        let ctor = match (role_of(&spi, i, "mosi"), role_of(&spi, i, "miso")) {
            (Some(mosi), Some(miso)) => format!(
                "new(\n        p.{peri},\n        Irqs,\n        p.{}, // SCK\n        p.{}, // MISO\n        p.{}, // MOSI\n        {var}_cfg,\n    )",
                periph(sck),
                periph(miso),
                periph(mosi)
            ),
            (Some(mosi), None) => format!(
                "new_txonly(\n        p.{peri},\n        Irqs,\n        p.{}, // SCK\n        p.{}, // MOSI\n        {var}_cfg,\n    )",
                periph(sck),
                periph(mosi)
            ),
            (None, Some(miso)) => format!(
                "new_rxonly(\n        p.{peri},\n        Irqs,\n        p.{}, // SCK\n        p.{}, // MISO\n        {var}_cfg,\n    )",
                periph(sck),
                periph(miso)
            ),
            (None, None) => {
                o.push_str(&format!(
                    "    // SPIM{i}: only SCK is wired. embassy-nrf builds a SPIM with MOSI, MISO or\n    // both, so wire one of them on the Pins canvas.\n"
                ));
                continue;
            }
        };
        if let Some(note) = clash(&taken, &peri, &format!("SPIM{i}")) {
            o.push_str(&note);
            continue;
        }
        let hz = bus_speed(mcu, "spim", i);
        let (got, variant) = spim_frequency(hz);
        if got != hz {
            o.push_str(&format!(
                "    // {hz} Hz asked for; the SPIM has fixed rates and {got} is the highest at or below it.\n"
            ));
        }
        let sc = scfgs.get(&i);
        let mode = sc.map_or(0, |c| c.mode).min(3);
        let order = match sc.map(|c| c.bit_order) {
            Some(SpiBitOrder::LsbFirst) => "LsbFirst",
            _ => "MsbFirst",
        };
        o.push_str(&format!(
            "    let mut {var}_cfg = embassy_nrf::spim::Config::default();\n    {var}_cfg.frequency = embassy_nrf::spim::Frequency::{variant};\n    {var}_cfg.mode = embassy_nrf::spim::MODE_{mode};\n    {var}_cfg.bit_order = embassy_nrf::spim::BitOrder::{order};\n"
        ));
        o.push_str(&format!(
            "{ALLOW}    let mut {var} = embassy_nrf::spim::Spim::{ctor};\n"
        ));
        irqs.push(format!(
            "    {irq} => embassy_nrf::spim::InterruptHandler<embassy_nrf::peripherals::{peri}>;"
        ));
        taken.push((peri, format!("SPIM{i}")));
    }

    let i2c = i2c_pins(mcu);
    let icfgs = modules::i2c_configs(&mcu.modules);
    for i in instances(&i2c) {
        let (Some(scl), Some(sda)) = (role_of(&i2c, i, "scl"), role_of(&i2c, i, "sda")) else {
            o.push_str(&format!(
                "    // TWIM{i}: SCL and SDA are taken together; wire the missing one.\n"
            ));
            continue;
        };
        let (peri, irq) = serial_block(&mcu.family, "twim", i);
        if let Some(note) = clash(&taken, &peri, &format!("TWIM{i}")) {
            o.push_str(&note);
            continue;
        }
        let var = format!("twim{i}");
        // Past every gate above, so this bus really is built: the const and the
        // driver appear together or not at all.
        let icfg = icfgs.get(&i);
        let stems = icfg.map_or_else(Vec::new, |c| {
            super::common::legacy_i2c_device_stems(&format!("twim{i}"), c)
        });
        if stems.is_empty() {
            items.push_str(&super::common::device_address_const(
                Some(&format!("TWIM{i}")),
                icfg.map_or(0, |c| c.primary_address()),
            ));
        } else {
            for (stem, addr, _) in stems {
                items.push_str(&super::common::device_address_const(
                    Some(&stem.to_ascii_uppercase()),
                    addr,
                ));
            }
        }
        let hz = bus_speed(mcu, "twim", i);
        let (got, variant) = twim_frequency(hz);
        if got != hz {
            o.push_str(&format!(
                "    // {hz} Hz asked for; the TWIM has fixed rates and {got} is the highest at or below it.\n"
            ));
        }
        o.push_str(&format!(
            "    let mut {var}_cfg = embassy_nrf::twim::Config::default();\n    {var}_cfg.frequency = embassy_nrf::twim::Frequency::{variant};\n"
        ));
        // Without it, `twim.write(addr, &[REG, 0x01])` fails at run time: a
        // slice of constants is promoted into flash, and EasyDMA reads RAM only.
        let up = var.to_ascii_uppercase();
        o.push_str("    // EasyDMA reads RAM only, and a write of constant bytes (`&[REG, 0x01]`)\n    // lives in flash, so the driver copies it through this buffer first; a longer\n    // one fails with `RAMBufferTooSmall`.");
        // On Async `'static`, so the bus can move into a task. On Blocking
        // `main` never returns, so a local outlives every use of the bus and
        // the project needs no `static_cell`.
        let ram = if mcu.is_async() {
            o.push_str(&format!(
                " `'static`, so the bus can move into a task.\n    static {up}_RAM: static_cell::StaticCell<[u8; 32]> = static_cell::StaticCell::new();\n"
            ));
            static_cell = true;
            format!("{up}_RAM.init([0; 32])")
        } else {
            o.push_str(&format!("\n    let mut {var}_ram = [0u8; 32];\n"));
            format!("&mut {var}_ram")
        };
        o.push_str(&format!(
            "{ALLOW}    let mut {var} = embassy_nrf::twim::Twim::new(\n        p.{peri},\n        Irqs,\n        p.{}, // SDA\n        p.{}, // SCL\n        {var}_cfg,\n        {ram},\n    );\n",
            periph(sda),
            periph(scl),
        ));
        irqs.push(format!(
            "    {irq} => embassy_nrf::twim::InterruptHandler<embassy_nrf::peripherals::{peri}>;"
        ));
        taken.push((peri, format!("TWIM{i}")));
    }

    let tcfgs = modules::timer_configs(&mcu.modules);
    for (inst, chans) in pwm_channels(mcu) {
        // `SimplePwm` has constructors for one to four pads, in order.
        let chans: Vec<(u8, (u8, u8))> = chans.into_iter().take(4).collect();
        let var = format!("pwm{inst}");
        let cfg = tcfgs.get(&inst);
        let want = cfg.map_or(0, |c| c.freq_hz);
        let counting = cfg.map_or(PwmCounting::EdgeUp, |c| c.counting);
        let center = !matches!(counting, PwmCounting::EdgeUp | PwmCounting::EdgeDown);
        // The field writes are collected first: with none, a `let mut` would
        // be a warning, and the matrix counts warnings.
        let mut fields = String::new();
        if center {
            fields.push_str("    // Center-aligned: the counter runs up to the top and back down, so one\n    // period is two tops long.\n");
            fields.push_str(&format!(
                "    {var}_cfg.counter_mode = embassy_nrf::pwm::CounterMode::UpAndDown;\n"
            ));
        } else if counting == PwmCounting::EdgeDown {
            fields.push_str("    // Edge-aligned down asked for; the nRF PWM counts up, or up and down, so\n    // this is edge-aligned up: the same pulse width, from the start of the period.\n");
        }
        if want > 0 {
            let (div, name, top, got) = pwm_timing(want, center);
            if got != want {
                fields.push_str(&format!(
                    "    // {want} Hz asked for; the pad sees {got} Hz.\n"
                ));
            }
            fields.push_str(&format!(
                "    // {} MHz / {div}: the smallest divider that keeps the top within 15 bits.\n    {var}_cfg.prescaler = embassy_nrf::pwm::Prescaler::{name};\n    {var}_cfg.max_duty = {top};\n",
                PWM_CLOCK_HZ / 1_000_000
            ));
        } else {
            fields.push_str(&format!(
                "    // No frequency set in the module: embassy-nrf's default stands (16 MHz / 16\n    // over a top of 1000, {} Hz). Set one in the Virtual Module.\n",
                if center { 500 } else { 1_000 }
            ));
        }
        for (k, (ch, _)) in chans.iter().enumerate() {
            if cfg.is_some_and(|c| c.channel_of(*ch).output == PwmOutput::OpenDrain) {
                fields.push_str(&format!(
                    "    {var}_cfg.ch{k}_drive = embassy_nrf::gpio::OutputDrive::Standard0Disconnect1;\n"
                ));
            }
        }
        let binding = if fields.contains(&format!("{var}_cfg.")) {
            "let mut"
        } else {
            "let"
        };
        o.push_str(&format!(
            "    {binding} {var}_cfg = embassy_nrf::pwm::SimpleConfig::default();\n{fields}"
        ));
        let pads: String = chans
            .iter()
            .map(|(ch, pp)| format!("        p.{}, // channel {ch}\n", periph(*pp)))
            .collect();
        o.push_str(&format!(
            "{ALLOW}    let mut {var} = embassy_nrf::pwm::SimplePwm::new_{}ch(\n        p.PWM{inst},\n{pads}        &{var}_cfg,\n    );\n",
            chans.len()
        ));
        o.push_str("    // `inverted(v)` holds the pad high while the counter is below v, so v / top\n    // is the high share; `normal(v)` holds it low for that share instead.\n");
        for (k, (ch, pp)) in chans.iter().enumerate() {
            let x100 = cfg.map_or(0, |c| c.duty_x100_of(*ch)).min(10_000);
            let shape = cfg.map(|c| c.channel_of(*ch)).unwrap_or_default();
            // Mode 2 reverses the comparison, which is a second inversion on
            // top of the polarity: the two cancel.
            let low = (shape.polarity == PwmPolarity::ActiveLow) != (shape.mode == PwmMode::Mode2);
            let (duty, level) = if low {
                ("normal", "low")
            } else {
                ("inverted", "high")
            };
            // `SimplePwm` numbers its outputs by the order the pads were given.
            let slot = if usize::from(*ch) == k {
                String::new()
            } else {
                format!(", output {k} here")
            };
            // The two ends written out: `x * 0` is a clippy error, and the top
            // itself is exact where the arithmetic would only be close.
            let value = match x100 {
                0 => "0".to_owned(),
                10_000 => format!("{var}.max_duty()"),
                _ => format!("({var}.max_duty() as u32 * {x100} / 10_000) as u16"),
            };
            o.push_str(&format!(
                "    // Channel {ch} on {}{slot}: {} % of the period {level}.\n    {var}.set_duty({k}, embassy_nrf::pwm::DutyCycle::{duty}({value}));\n",
                label(*pp),
                super::common::duty_percent_str(x100)
            ));
        }
    }

    let mut adc: Vec<(u8, (u8, u8))> = mcu
        .iter_all_pins()
        .filter(|p| !p.reserved)
        .filter_map(|p| match p.selected_function {
            PinFunction::AdcChannel { channel, .. } => nrf_pin(&p.name).map(|pp| (channel, pp)),
            _ => None,
        })
        .collect();
    adc.retain(|a| block_present(mcu, "saadc", Some(a.0)));
    adc.sort_unstable();
    if !adc.is_empty() {
        let order: Vec<String> = adc
            .iter()
            .map(|(ch, pp)| format!("AIN{ch} ({})", label(*pp)))
            .collect();
        o.push_str(&format!(
            "    // One SAADC for every analog input. `let mut buf = [0i16; {}];` then\n    // `saadc.sample(&mut buf).await` fills it in this order: {}.\n",
            adc.len(),
            order.join(", ")
        ));
        let inputs: String = adc
            .iter()
            .map(|(_, pp)| {
                format!(
                    "            embassy_nrf::saadc::ChannelConfig::single_ended(p.{}),\n",
                    periph(*pp)
                )
            })
            .collect();
        o.push_str(&format!(
            "{ALLOW}    let mut saadc = embassy_nrf::saadc::Saadc::new(\n        p.SAADC,\n        Irqs,\n        embassy_nrf::saadc::Config::default(),\n        [\n{inputs}        ],\n    );\n"
        ));
        irqs.push("    SAADC => embassy_nrf::saadc::InterruptHandler;".to_owned());
    }

    let mut spawns = false;
    if usb_wired(mcu) && !mcu.is_async() {
        // The part nrf-hal has no crate for, on Blocking: embassy-nrf's USB
        // driver only runs under embassy-usb, which needs the executor.
        o.push_str("    // USB is NOT built: on this part Blocking is embassy-nrf, whose USB driver\n    // runs under embassy-usb and needs the executor. Switch to the Async runtime.\n");
    } else if usb_wired(mcu) {
        // The device runs in its own task forever; the class stays in `main`.
        // Every buffer the builder keeps is `'static`, because the device it
        // builds moves into that task.
        items.push_str("/// Runs the USB device: enumeration, control requests, suspend and resume.\n/// It never returns; the CDC class in `main` does the talking.\n#[embassy_executor::task]\nasync fn usb_task(\n    mut device: embassy_usb::UsbDevice<\n        'static,\n        embassy_nrf::usb::Driver<'static, embassy_nrf::usb::vbus_detect::HardwareVbusDetect>,\n    >,\n) -> ! {\n    device.run().await\n}\n\n");
        o.push_str("\n    // ── USB (USBD) ──\n");
        o.push_str(&usb_identity(mcu));
        o.push_str("    let usb_driver = embassy_nrf::usb::Driver::new(\n        p.USBD,\n        Irqs,\n        embassy_nrf::usb::vbus_detect::HardwareVbusDetect::new(Irqs),\n    );\n");
        o.push_str("    let mut usb_config = embassy_usb::Config::new(USB_VID, USB_PID);\n    usb_config.product = Some(USB_PRODUCT);\n    usb_config.max_power = 100;\n    usb_config.max_packet_size_0 = 64;\n");
        o.push_str("    static USB_CONFIG_DESC: static_cell::StaticCell<[u8; 256]> = static_cell::StaticCell::new();\n    static USB_BOS_DESC: static_cell::StaticCell<[u8; 256]> = static_cell::StaticCell::new();\n    static USB_MSOS_DESC: static_cell::StaticCell<[u8; 256]> = static_cell::StaticCell::new();\n    static USB_CONTROL: static_cell::StaticCell<[u8; 64]> = static_cell::StaticCell::new();\n    static USB_CDC_STATE: static_cell::StaticCell<embassy_usb::class::cdc_acm::State> =\n        static_cell::StaticCell::new();\n");
        o.push_str("    let mut usb_builder = embassy_usb::Builder::new(\n        usb_driver,\n        usb_config,\n        USB_CONFIG_DESC.init([0; 256]),\n        USB_BOS_DESC.init([0; 256]),\n        USB_MSOS_DESC.init([0; 256]),\n        USB_CONTROL.init([0; 64]),\n    );\n");
        o.push_str("    // A CDC serial port: `usb_serial.wait_connection().await`, then\n    // `read_packet` / `write_packet`, 64 bytes at a time.\n");
        o.push_str(&format!(
            "{ALLOW}    let mut usb_serial = embassy_usb::class::cdc_acm::CdcAcmClass::new(\n        &mut usb_builder,\n        USB_CDC_STATE.init(embassy_usb::class::cdc_acm::State::new()),\n        64,\n    );\n"
        ));
        o.push_str("    spawner.spawn(usb_task(usb_builder.build()).unwrap());\n");
        irqs.push(
            "    USBD => embassy_nrf::usb::InterruptHandler<embassy_nrf::peripherals::USBD>;".to_owned(),
        );
        // VBUS detection sits on the power block's vector on an nRF52 and on
        // the USB regulator's on the nRF5340.
        let vbus = chip(&mcu.family).map_or("CLOCK_POWER", |c| c.vbus_irq());
        irqs.push(format!(
            "    {vbus} => embassy_nrf::usb::vbus_detect::InterruptHandler;"
        ));
        static_cell = true;
        spawns = true;
    }

    if let Some((sck, csn, io)) = qspi_wired(mcu).filter(|_| mcu.is_async()) {
        let d = crate::panels::mcu_module::modules::QspiModuleConfig::new(1);
        let c = modules::qspi_config(&mcu.modules).unwrap_or(d);
        let (hz, freq) = qspi_frequency(c.prescaler);
        // embassy's sizes run 1 KiB << i; `capacity` is a u32 of bytes.
        let bytes = (1024u64 << c.memory_size.min(22)).min(u64::from(u32::MAX));
        o.push_str("\n    // ── QSPI ──\n");
        o.push_str("    let mut qspi_cfg = embassy_nrf::qspi::Config::default();\n");
        if c.prescaler > 15 {
            o.push_str(&format!(
                "    // Prescaler {} asked for; the nRF QSPI divides 32 MHz by 16 at most.\n",
                c.prescaler
            ));
        }
        o.push_str(&format!(
            "    // {} of flash, the size `embedded-storage` reports.\n    qspi_cfg.capacity = {bytes};\n",
            c.memory_size_label()
        ));
        o.push_str(&format!(
            "    // 32 MHz / {}: {} Hz on SCK.\n    qspi_cfg.frequency = embassy_nrf::qspi::Frequency::{freq};\n",
            32_000_000 / hz,
            hz
        ));
        use crate::panels::mcu_module::modules::QspiAddressSize;
        let addr = match c.address_size {
            QspiAddressSize::Bits32 => "_32bit",
            QspiAddressSize::Bits24 => "_24bit",
            other => {
                o.push_str(&format!(
                    "    // {} addressing asked for; the nRF QSPI addresses in 24 or 32 bits.\n",
                    other.label()
                ));
                "_24bit"
            }
        };
        o.push_str(&format!(
            "    qspi_cfg.address_mode = embassy_nrf::qspi::AddressMode::{addr};\n"
        ));
        o.push_str("    // Quad I/O read and page program by default - Config::default()'s opcodes.\n");
        o.push_str(&format!(
            "{ALLOW}    let mut qspi = embassy_nrf::qspi::Qspi::new(\n        p.QSPI,\n        Irqs,\n        p.{}, // SCK\n        p.{}, // CSN\n        p.{}, // IO0\n        p.{}, // IO1\n        p.{}, // IO2\n        p.{}, // IO3\n        qspi_cfg,\n    );\n",
            periph(sck),
            periph(csn),
            periph(io[0]),
            periph(io[1]),
            periph(io[2]),
            periph(io[3]),
        ));
        irqs.push(
            "    QSPI => embassy_nrf::qspi::InterruptHandler<embassy_nrf::peripherals::QSPI>;"
                .to_owned(),
        );
    } else if qspi_touched(mcu) && block_present(mcu, "qspi", None) && mcu.is_async() {
        o.push_str("    // QSPI is NOT built: the driver is quad-only and takes SCK, CSN and all four\n    // IO lanes. Wire the missing ones on the Pins canvas.\n");
    }

    AsyncBuses {
        irqs,
        body: o,
        items,
        static_cell,
        spawns,
    }
}

/// Whether the Async project needs `static_cell` in its manifest: a TWIM's
/// RAM copy buffer is `'static`.
///
/// Answered by RUNNING the bus pass, like `rp::dma_uses`, rather than by a
/// second reading of the pins: a TWIM that loses its block to a SPIM writes no
/// buffer, and a separate rule would miss that.
pub fn needs_static_cell(mcu: &Mcu) -> bool {
    mcu.is_async() && is_nrf(&mcu.family) && async_bus_lines(mcu).static_cell
}

/// The module that shares `me`'s serial block, as its peripheral name.
///
/// SPIMn and TWIMn (n < 2) are ONE block on the nRF52 - `serial_block` names
/// it `TWISPIn` - and the chip runs one of them at a time. `async_bus_lines`
/// builds the SPIM and leaves the TWIM a comment; `bus_lines` builds both,
/// and the TWIM's `init`, second in `main.rs`, writes the block's shared
/// ENABLE register over the SPIM's. Either way one bus is silently lost, so
/// the modules panel asks this per module and says so on both. `Some("TWIM0")`
/// for the SPI of such a pair, `Some("SPIM0")` for the I2C, `None` on any
/// other chip, instance or kind.
///
/// Read off the PADS, and only for a pair the generator would really build
/// on `is_async`'s runtime: each loop skips a half-wired bus before its clash
/// check, so with one half short the other has the block to itself, and the
/// panel's sentence about which one loses would be false. The tests hold this
/// to `fresh_main_rs` for both runtimes.
pub fn shared_block_partner(
    mcu: &Mcu,
    me: &crate::panels::mcu_module::modules::VirtualModule,
    is_async: bool,
) -> Option<String> {
    use crate::panels::mcu_module::modules::ModuleKind;
    if !is_nrf(&mcu.family) {
        return None;
    }
    let family = mcu.family.as_str();
    let n = me.instance();
    // The partner is the instance of the OTHER kind on the same block, and
    // which one that is depends on the part: TWIM0 shares with SPIM1 on the
    // 52811, and with nothing on the 52805/52810.
    let (sn, tn) = match me.kind {
        ModuleKind::GenericInterfaceSpi => {
            let block = serial_block(family, "spim", n).0;
            (n, (0..4).find(|&m| serial_block(family, "twim", m).0 == block)?)
        }
        ModuleKind::GenericInterfaceI2c => {
            let block = serial_block(family, "twim", n).0;
            ((0..4).find(|&m| serial_block(family, "spim", m).0 == block)?, n)
        }
        _ => return None,
    };
    let name = match me.kind {
        ModuleKind::GenericInterfaceSpi => format!("TWIM{tn}"),
        _ => format!("SPIM{sn}"),
    };
    // embassy-nrf has no clock-only constructor; on the part nrf-hal has no
    // crate for, Blocking is embassy-nrf too.
    let embassy = is_async || blocking_on_embassy(family);
    // The same tests, in the same order, as the two SPIM loops: SCK first,
    // then (embassy only) a data line, since nrf-hal takes MOSI and MISO as
    // `Option`s.
    let spi = spi_pins(mcu);
    let spim = role_of(&spi, sn, "sck").is_some()
        && (!embassy || role_of(&spi, sn, "mosi").is_some() || role_of(&spi, sn, "miso").is_some());
    // Both TWIM loops take the pair or nothing.
    let i2c = i2c_pins(mcu);
    let twim = role_of(&i2c, tn, "scl").is_some() && role_of(&i2c, tn, "sda").is_some();
    (spim && twim).then_some(name)
}

/// The block on embassy-nrf: under the executor on Async, and under a plain
/// `#[entry]` on Blocking for the part nrf-hal has no crate for.
fn async_section(mcu: &Mcu) -> String {
    let buses = async_bus_lines(mcu);
    let (tasks, gpio) = async_gpio_lines(mcu);
    // An armed input is the only thing here that needs the spawner.
    let spawner = if tasks.is_empty() && !buses.spawns {
        "_spawner"
    } else {
        "spawner"
    };
    let mut o = String::new();
    o.push_str(GEN_BEGIN);
    o.push('\n');
    if !buses.irqs.is_empty() {
        o.push_str(&format!(
            "// The handler each async peripheral needs on its interrupt vector.\nembassy_nrf::bind_interrupts!(struct Irqs {{\n{}\n}});\n\n",
            buses.irqs.join("\n")
        ));
    }
    o.push_str(&buses.items);
    o.push_str(&tasks);
    if mcu.is_async() {
        o.push_str("#[embassy_executor::main]\n");
        o.push_str(&format!(
            "async fn main({spawner}: embassy_executor::Spawner) {{\n"
        ));
        o.push_str("    // Imported here, not in the header: a runtime switch keeps the header, and\n    // on Blocking embassy-nrf is not a dependency.\n");
    } else {
        // The part nrf-hal has no crate for: embassy-nrf with no executor,
        // through its `blocking_*` methods.
        o.push_str("#[cortex_m_rt::entry]\n");
        o.push_str("fn main() -> ! {\n");
        o.push_str("    // embassy-nrf without an executor: nrf-hal is not used for this part, so\n    // Blocking calls the drivers' `blocking_*` methods (`blocking_write`, ...).\n");
    }
    o.push_str("    #[allow(unused_imports)]\n    use embassy_nrf::gpio::{Input, Level, Output, OutputDrive, Pull};\n\n");
    o.push_str(&async_clock_lines(mcu));
    // Only the peripheral: starting it is configuring it on this HAL.
    o.push_str(&super::watchdog_gen::nrf_init_lines(
        &mcu.watchdog,
        true,
        chip(&mcu.family).map_or("WDT", |c| c.wdt()),
    ));
    o.push_str(&async_nfc_note(mcu));
    o.push_str(&gpio);
    if !gpio.is_empty() && !buses.body.is_empty() {
        o.push('\n');
    }
    o.push_str(&buses.body);
    o.push_str(GEN_END);
    o.push('\n');
    o
}

impl FamilyBackend for AsyncNrfBackend {
    /// A LABEL: dispatch is by runtime, through `backend_for_runtime`.
    fn family_id(&self) -> &'static str {
        "nrf-async"
    }

    fn handles(&self, family: &str) -> bool {
        is_nrf(family)
    }

    // `gpio_modes` is the trait default here too: `Input::new` takes all three
    // pulls and `OutputDrive` has open-drain, so the full set is what is emitted.

    /// Only the watchdog: every bus on this runtime is built inline in
    /// main.rs. Without this the file would never be written.
    fn config_files(&self, mcu: &Mcu) -> Vec<(String, String)> {
        super::watchdog_gen::nrf_config_files(&mcu.watchdog, &mcu.family, true)
    }

    fn fresh_main_rs(&self, mcu: &Mcu) -> String {
        format!(
            "{}{}{ASYNC_USER_TAIL}",
            header(mcu, true),
            async_section(mcu)
        )
    }

    /// The blocking backend's splice, with this runtime's block, provenance
    /// line and tail.
    fn update_main_rs(&self, mcu: &Mcu, existing: &str) -> String {
        let (Some(begin), Some(end_start)) = (existing.find(GEN_BEGIN), existing.find(GEN_END))
        else {
            return self.fresh_main_rs(mcu);
        };
        let end = end_start + GEN_END.len();
        format!(
            "{}{}{}",
            refresh_hal_line(&existing[..begin], &hal_line(mcu, true)),
            async_section(mcu).trim_end_matches('\n'),
            retarget_pristine_tail(&existing[end..], true)
        )
    }

    /// One per armed input: the hook its task calls. The Blocking backend
    /// generates no handler and keeps the default, so a switch to Blocking
    /// leaves a seeded hook alone as dead code, and a switch back calls it.
    fn edge_hooks(&self, mcu: &Mcu) -> Vec<EdgeHook> {
        async_edge_hooks(mcu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nrf_parts_are_nrf_and_nothing_else_is() {
        assert!(is_nrf("nrf52833"));
        assert!(is_nrf("nrf52840"));
        assert!(is_nrf("nrf5340"));
        assert!(is_nrf("nrf54l15"));
        for other in ["nrf51", "nrf5340-net", "nrf54h20", "rp2040", "stm32f4", "esp32c3", ""] {
            assert!(!is_nrf(other), "{other:?} matched");
        }
    }

    /// The BBC micro:bit v2 definition must parse, build, and keep the facts
    /// the rest of the IDE reads straight off it. Not a built-in yet, so no
    /// other test loads it - and a `.ron` nobody loads is documentation that
    /// rots.
    #[test]
    fn the_microbit_v2_definition_parses_and_builds() {
        use crate::panels::mcu_module::mcu_catalog::ToolchainKind;
        use crate::panels::mcu_module::mcu_def::McuDefinition;

        const SRC: &str = include_str!("../../../../assets/mcus/nrf52833_microbit_v2.ron");
        let def: McuDefinition =
            ron::from_str(SRC).expect("the micro:bit v2 definition must parse");

        assert!(is_nrf(&def.family));
        assert_eq!(def.board_chip.as_deref(), Some("nRF52833"));
        assert_eq!(def.toolchain, ToolchainKind::RustEmbedded);
        let p = &def.project;
        assert_eq!(p.target, "thumbv7em-none-eabihf");
        assert_eq!(p.probe_chip, "nRF52833_xxAA");
        assert_eq!(
            (p.flash_origin.as_str(), p.flash_size.as_str()),
            ("0x00000000", "512K")
        );
        assert_eq!(
            (p.ram_origin.as_str(), p.ram_size.as_str()),
            ("0x20000000", "128K")
        );
        assert!(
            p.hal_dep_async
                .as_deref()
                .is_some_and(|l| l.contains("nfc-pins-as-gpio")),
            "pads 8 and 9 do not exist in embassy-nrf without it"
        );

        assert_eq!(def.pins.bottom.len(), 25, "25 edge-connector contacts");
        let mcu = def.build_mcu();
        assert_eq!(mcu.iter_all_pins().count(), 25 + 14);

        let mut numbers: Vec<usize> = mcu.iter_all_pins().map(|p| p.number).collect();
        numbers.sort_unstable();
        numbers.dedup();
        assert_eq!(numbers.len(), 39, "pin numbers must be unique");
    }

    /// Every GPIO pad names its nRF port and pin first, and no GPIO is claimed
    /// twice. A duplicated `P0.xx` would generate two owners for one pin.
    #[test]
    fn every_gpio_pad_names_a_distinct_nrf_pin() {
        use crate::panels::mcu_module::mcu_def::McuDefinition;

        const SRC: &str = include_str!("../../../../assets/mcus/nrf52833_microbit_v2.ron");
        let def: McuDefinition = ron::from_str(SRC).unwrap();
        let mcu = def.build_mcu();

        let mut seen = std::collections::BTreeSet::new();
        for pin in mcu.iter_all_pins() {
            let head = pin.name.split_whitespace().next().unwrap_or("");
            let is_gpio = head.len() == 5
                && (head.starts_with("P0.") || head.starts_with("P1."))
                && head[3..].chars().all(|c| c.is_ascii_digit());
            if !is_gpio {
                assert!(
                    pin.reserved,
                    "{}: not a GPIO, so it must be reserved",
                    pin.name
                );
                continue;
            }
            assert!(seen.insert(head.to_owned()), "{head} appears twice");
        }
        assert_eq!(
            seen.len(),
            19 + 14,
            "19 GPIO contacts plus 14 on-board nets"
        );
    }

    /// Where autowire puts each bus on this board, stated as it is today.
    ///
    /// SPI lands on pads 13/14/15, the only pads that offer it. I2C and UART
    /// are different: the internal sensor bus and the USB-serial nets offer
    /// them too, and autowire picks those FIRST. For UART that is the default
    /// most users want (serial to the PC). For I2C it is right for the on-board
    /// motion sensor and wrong for an external device, which then needs the
    /// second pick - pads 19/20 - once the internal pads are taken.
    ///
    /// Open until on-board device modules exist to claim the internal nets
    /// explicitly; at that point a plain I2C module should prefer the edge.
    #[test]
    fn where_autowire_puts_each_bus_on_the_board() {
        use crate::panels::mcu_module::mcu_def::McuDefinition;
        use crate::panels::mcu_module::modules::ModuleSignal as S;
        use crate::panels::mcu_module::modules::autowire::pick_pins;
        use std::collections::HashSet;

        const SRC: &str = include_str!("../../../../assets/mcus/nrf52833_microbit_v2.ron");
        let def: McuDefinition = ron::from_str(SRC).unwrap();
        let mcu = def.build_mcu();
        let name_of = |n: usize| mcu.find_pin(n).map(|p| p.name.clone()).unwrap();
        let none = HashSet::new();

        let (inst, spi) = pick_pins(
            &mcu,
            &none,
            &HashSet::new(),
            &[S::Sck, S::Mosi, S::Miso],
            &[],
        )
        .expect("SPI must wire");
        assert_eq!(inst, 2, "SPIM2, the one that shares no ID with a TWIM");
        let spi: Vec<String> = spi.into_iter().map(|(_, n)| name_of(n)).collect();
        for want in ["pad 13", "pad 14", "pad 15"] {
            assert!(
                spi.iter().any(|n| n.contains(want)),
                "SPI missed {want}: {spi:?}"
            );
        }

        // First picks: the on-board nets.
        let (_, first) =
            pick_pins(&mcu, &none, &HashSet::new(), &[S::Scl, S::Sda], &[]).expect("I2C must wire");
        let first: Vec<String> = first.into_iter().map(|(_, n)| name_of(n)).collect();
        assert!(
            first.iter().all(|n| n.contains("INT_")),
            "I2C first pick: {first:?}"
        );
        let (_, uart) =
            pick_pins(&mcu, &none, &HashSet::new(), &[S::Tx, S::Rx], &[]).expect("UART must wire");
        let uart: Vec<String> = uart.into_iter().map(|(_, n)| name_of(n)).collect();
        for want in ["P0.06 (UART_TX)", "P1.08 (UART_RX)"] {
            assert!(uart.iter().any(|n| n == want), "UART first pick: {uart:?}");
        }

        // With the sensor bus taken, I2C moves to the labeled edge pads.
        let used: HashSet<usize> = mcu
            .iter_all_pins()
            .filter(|p| p.name.contains("INT_"))
            .map(|p| p.number)
            .collect();
        let (_, i2c) =
            pick_pins(&mcu, &used, &HashSet::new(), &[S::Scl, S::Sda], &[]).expect("I2C must wire");
        let i2c: Vec<String> = i2c.into_iter().map(|(_, n)| name_of(n)).collect();
        for want in ["pad 19", "pad 20"] {
            assert!(
                i2c.iter().any(|n| n.contains(want)),
                "I2C missed {want}: {i2c:?}"
            );
        }
    }

    /// HFCLK is 64 MHz from either source, and LFCLK is 32.768 kHz from either
    /// of the two the board can use. There is no LFXO node: the crystal is not
    /// fitted, and offering it would generate a `start_lfclk()` that never
    /// returns.
    #[test]
    fn the_clock_graph_delivers_64_mhz_and_32768_hz_from_every_source() {
        use crate::panels::mcu_module::clock::graph::eval::evaluate;
        use crate::panels::mcu_module::clock::graph::model::NodeState;
        use crate::panels::mcu_module::mcu_def::{ClockDef, McuDefinition};

        const SRC: &str = include_str!("../../../../assets/mcus/nrf52833_microbit_v2.ron");
        let def: McuDefinition = ron::from_str(SRC).unwrap();
        let ClockDef::Graph(gc) = def.effective_clock() else {
            panic!("the definition carries its own graph");
        };
        assert!(
            gc.graph.node("lfxo").is_none(),
            "no 32.768 kHz crystal on the board"
        );

        for hf in 0..2 {
            for lf in 0..2 {
                let mut g = gc.graph.clone();
                g.node_mut("hfclk_src").unwrap().state = NodeState::Index(hf);
                g.node_mut("lfclk_src").unwrap().state = NodeState::Index(lf);
                let hz = evaluate(&g);
                assert_eq!(hz["hfclk"], 64_000_000, "hfclk_src={hf}");
                assert_eq!(hz["lfclk"], 32_768, "hfclk_src={hf} lfclk_src={lf}");
            }
        }
    }
}

#[cfg(test)]
mod blocking_codegen {
    use crate::panels::mcu_module::clock::graph::model::NodeState;
    use crate::panels::mcu_module::clock::model::ClockConfig;
    use crate::panels::mcu_module::mcu::Mcu;
    use crate::panels::mcu_module::pins::PinFunction;
    use crate::panels::mcu_module::pins::logic::pin::GpioMode;
    use crate::panels::mcu_module::{builtins, project_gen};

    /// The built-in micro:bit, wired as `wire` says, by pad name prefix.
    pub(super) fn microbit(wire: &[(&str, PinFunction)]) -> Mcu {
        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "nrf52833_microbit_v2")
            .expect("built-in micro:bit v2")
            .build_mcu();
        for p in mcu.iter_all_pins_mut() {
            if let Some((_, f)) = wire.iter().find(|(head, _)| p.name.starts_with(head)) {
                p.selected_function = f.clone();
            }
        }
        mcu
    }

    /// One of everything, on the pads the board labels for it.
    pub(super) fn everything() -> Mcu {
        let mut mcu = microbit(&[
            ("P0.21", PinFunction::GpioOutput),
            ("P0.14", PinFunction::GpioInput),
            ("P0.06", PinFunction::UsartTx(0)),
            ("P1.08", PinFunction::UsartRx(0)),
            ("P0.08", PinFunction::I2cScl(0)),
            ("P0.16", PinFunction::I2cSda(0)),
            ("P0.17", PinFunction::SpiSck(2)),
            ("P0.13", PinFunction::SpiMosi(2)),
            ("P0.01", PinFunction::SpiMiso(2)),
            (
                "P0.00",
                PinFunction::TimerPwm {
                    timer: 0,
                    channel: 0,
                },
            ),
            (
                "P1.02",
                PinFunction::TimerPwm {
                    timer: 0,
                    channel: 1,
                },
            ),
            ("P0.02", PinFunction::AdcChannel { adc: 0, channel: 0 }),
        ]);
        mcu.reconcile_modules();
        for m in &mut mcu.modules {
            if let crate::panels::mcu_module::modules::ModuleConfig::Timer(c) = &mut m.config {
                c.freq_hz = 50;
                c.set_duty_x100(0, 750);
                c.set_duty_x100(1, 1_000);
            }
        }
        mcu
    }

    /// Set a mux on the tree to `idx`.
    pub(super) fn select(mcu: &mut Mcu, mux: &str, idx: usize) {
        let ClockConfig::Graph(gc) = &mut mcu.clock else {
            panic!("the micro:bit carries a graph");
        };
        gc.graph.node_mut(mux).unwrap().state = NodeState::Index(idx);
    }

    /// The tree's two muxes become the two `Clocks` calls, and nothing else
    /// does: the internal choices emit no `enable_ext_hfosc`, the crystal
    /// choice does, and LFCLK follows its own mux.
    #[test]
    fn the_clock_block_follows_the_two_muxes() {
        let mut mcu = microbit(&[]);
        let main = mcu.fresh_main_rs();
        assert!(!main.contains("enable_ext_hfosc"), "{main}");
        assert!(main.contains(".set_lfclk_src_rc()"), "{main}");
        assert!(main.contains(".start_lfclk();"), "{main}");
        assert!(!main.contains("lfxo"), "{main}");

        select(&mut mcu, "hfclk_src", 1);
        select(&mut mcu, "lfclk_src", 1);
        let main = mcu.fresh_main_rs();
        assert!(main.contains(".enable_ext_hfosc()"), "{main}");
        assert!(main.contains(".set_lfclk_src_synth()"), "{main}");
        assert!(!main.contains("set_lfclk_src_rc"), "{main}");
    }

    /// Every wired pad reaches `main.rs` as the call its HAL wants, each bus
    /// through its config file, and the ADC on the TYPED pin.
    #[test]
    fn a_wired_board_generates_every_peripheral() {
        let mcu = everything();
        let main = mcu.fresh_main_rs();
        for want in [
            "#[cortex_m_rt::entry]",
            "nrf52833_hal::pac::Peripherals::take()",
            "let port0 = nrf52833_hal::gpio::p0::Parts::new(p.P0);",
            "let port1 = nrf52833_hal::gpio::p1::Parts::new(p.P1);",
            "let mut p0_21_out = port0.p0_21.into_push_pull_output(nrf52833_hal::gpio::Level::Low);",
            "let p0_14_in = port0.p0_14.into_floating_input();",
            "pins::configs::uarte0::init(\n        p.UARTE0,\n        port0.p0_06.into_push_pull_output(nrf52833_hal::gpio::Level::High).degrade(),\n        port1.p1_08.into_floating_input().degrade(),\n        None,\n        None,",
            "pins::configs::twim0::init(\n        p.TWIM0,\n        port0.p0_08.into_floating_input().degrade(),\n        port0.p0_16.into_floating_input().degrade(),",
            "pins::configs::spim2::init(\n        p.SPIM2,\n        port0.p0_17.into_push_pull_output(nrf52833_hal::gpio::Level::Low).degrade(),\n        Some(port0.p0_13",
            "Some(port0.p0_01.into_floating_input().degrade()),",
            "pins::configs::pwm0::init(\n        p.PWM0,\n        port0.p0_00.into_push_pull_output(nrf52833_hal::gpio::Level::Low).degrade(),\n        port1.p1_02",
            "let mut saadc = nrf52833_hal::saadc::Saadc::new(p.SAADC, nrf52833_hal::saadc::SaadcConfig::default());",
            "let mut p0_02_adc0_in0 = port0.p0_02.into_floating_input();",
        ] {
            assert!(main.contains(want), "missing {want:?} in:\n{main}");
        }
        // The board's own names ride along as comments.
        assert!(main.contains("// P0.21 (ROW1)"), "{main}");
        assert!(main.contains("// P0.14 (pad 5, BTN_A)"), "{main}");

        let files = mcu.config_files();
        let mut names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["pwm0.rs", "spim2.rs", "twim0/mod.rs", "uarte0.rs"]);
    }

    /// The pull and drive chosen in the pin panel pick the `into_*` method,
    /// an unset mode generates the FIRST one the panel offers (which is what
    /// the panel highlights), and a mode left over from the other direction
    /// is ignored.
    #[test]
    fn the_io_mode_picks_the_constructor() {
        let mut mcu = microbit(&[
            ("P0.21", PinFunction::GpioOutput),
            ("P0.14", PinFunction::GpioInput),
            ("P0.23", PinFunction::GpioInput),
            ("P0.11", PinFunction::GpioInput),
            ("P0.22", PinFunction::GpioOutput),
        ]);
        for p in mcu.iter_all_pins_mut() {
            match p.name.split_whitespace().next() {
                Some("P0.21") => p.io_mode = Some(GpioMode::OpenDrain),
                Some("P0.14") => p.io_mode = Some(GpioMode::PullUp),
                Some("P0.23") => p.io_mode = Some(GpioMode::PullDown),
                // Stale: an output mode on a pin that is now an input, and an
                // input mode on an output.
                Some("P0.11") => p.io_mode = Some(GpioMode::OpenDrain),
                Some("P0.22") => p.io_mode = Some(GpioMode::PullDown),
                _ => {}
            }
        }
        let main = mcu.fresh_main_rs();
        assert!(
            main.contains("port0.p0_21.into_open_drain_output(nrf52833_hal::gpio::OpenDrainConfig::Standard0Disconnect1, nrf52833_hal::gpio::Level::High)"),
            "{main}"
        );
        assert!(main.contains("port0.p0_14.into_pullup_input()"), "{main}");
        assert!(main.contains("port0.p0_23.into_pulldown_input()"), "{main}");
        assert!(main.contains("port0.p0_11.into_floating_input()"), "{main}");
        assert!(
            main.contains("port0.p0_22.into_push_pull_output(nrf52833_hal::gpio::Level::Low)"),
            "{main}"
        );

        // The panel's first chip and the generated default are the same thing.
        use crate::panels::mcu_module::codegen::family::gpio_modes_for;
        assert_eq!(
            gpio_modes_for(&mcu, &PinFunction::GpioInput).first(),
            Some(&GpioMode::for_input(None))
        );
        assert_eq!(
            gpio_modes_for(&mcu, &PinFunction::GpioOutput).first(),
            Some(&GpioMode::for_output(None))
        );
    }

    /// Half a bus is a comment naming the missing pad, and no config file.
    #[test]
    fn half_wired_buses_say_so_and_get_no_file() {
        let mcu = microbit(&[
            ("P0.06", PinFunction::UsartTx(0)),
            ("P0.08", PinFunction::I2cScl(0)),
            // A SPIM with only MOSI: SCK is the one pad it cannot do without.
            ("P0.13", PinFunction::SpiMosi(2)),
        ]);
        let main = mcu.fresh_main_rs();
        assert!(
            main.contains("// UARTE0: only one of TXD/RXD is wired"),
            "{main}"
        );
        assert!(
            main.contains("// TWIM0: SCL and SDA are taken together"),
            "{main}"
        );
        assert!(main.contains("// SPIM2: SCK is not wired"), "{main}");
        assert!(!main.contains("::init("), "{main}");
        assert!(mcu.config_files().is_empty());

        // SCK alone IS a bus on this chip: MOSI and MISO are options.
        let mcu = microbit(&[("P0.17", PinFunction::SpiSck(2))]);
        let main = mcu.fresh_main_rs();
        assert!(
            main.contains("pins::configs::spim2::init(\n        p.SPIM2,\n        port0.p0_17.into_push_pull_output(nrf52833_hal::gpio::Level::Low).degrade(),\n        None,\n        None,"),
            "{main}"
        );
    }

    /// CTS and RTS ride along into the UARTE when wired.
    #[test]
    fn flow_control_pads_reach_the_constructor() {
        let mcu = microbit(&[
            ("P0.06", PinFunction::UsartTx(0)),
            ("P1.08", PinFunction::UsartRx(0)),
            ("P0.02", PinFunction::UsartCts(0)),
            ("P0.03", PinFunction::UsartRts(0)),
        ]);
        let main = mcu.fresh_main_rs();
        assert!(
            main.contains("Some(port0.p0_02.into_floating_input().degrade()),\n        Some(port0.p0_03.into_push_pull_output(nrf52833_hal::gpio::Level::High).degrade()),"),
            "{main}"
        );
    }

    /// Two pads on one signal: the lower pin is configured and both are named.
    #[test]
    fn two_pads_on_one_signal_are_both_named() {
        let mcu = microbit(&[
            ("P0.06", PinFunction::UsartTx(0)),
            ("P0.02", PinFunction::UsartTx(0)),
            ("P1.08", PinFunction::UsartRx(0)),
        ]);
        let main = mcu.fresh_main_rs();
        assert!(
            main.contains("// UARTE0 TXD is wired to P0.02 and P0.06. Only P0.02 is configured:"),
            "{main}"
        );
        assert!(main.contains("port0.p0_02.into_push_pull_output"), "{main}");
        assert!(
            !main.contains("port0.p0_06.into_push_pull_output"),
            "{main}"
        );
    }

    /// Anything on the NFC pads gets the UICR note; nothing else does.
    #[test]
    fn using_an_nfc_pad_is_flagged() {
        let mcu = microbit(&[("P0.10", PinFunction::GpioInput)]);
        let main = mcu.fresh_main_rs();
        assert!(
            main.contains("// P0.10 is one of the NFC antenna pins"),
            "{main}"
        );
        assert!(main.contains("NFCPINS"), "{main}");

        let mcu = microbit(&[("P0.14", PinFunction::GpioInput)]);
        assert!(!mcu.fresh_main_rs().contains("NFCPINS"));
    }

    /// A definition with no P1 pin (an nRF52832-style part) takes only port 0:
    /// its HAL has no `p1` module, so a `port1` line would not compile.
    #[test]
    fn a_single_port_part_takes_only_port_zero() {
        let mut mcu = microbit(&[("P0.21", PinFunction::GpioOutput)]);
        for p in mcu.iter_all_pins_mut() {
            if let Some(rest) = p.name.strip_prefix("P1.") {
                p.name = format!("NC{rest}");
            }
        }
        let main = mcu.fresh_main_rs();
        assert!(
            main.contains("let port0 = nrf52833_hal::gpio::p0::Parts::new(p.P0);"),
            "{main}"
        );
        assert!(!main.contains("port1"), "{main}");
        assert!(main.contains("// The port, taken once."), "{main}");
        assert!(main.contains("port0.p0_21.into_push_pull_output"), "{main}");

        // And the stock micro:bit, which has P1 pads, takes both.
        let main = microbit(&[("P0.21", PinFunction::GpioOutput)]).fresh_main_rs();
        assert!(
            main.contains("let port1 = nrf52833_hal::gpio::p1::Parts::new(p.P1);"),
            "{main}"
        );
        assert!(main.contains("// Both ports, taken once."), "{main}");
    }

    /// The module's numbers land in the config files as the HAL's fixed
    /// settings: nearest baud, highest rate at or below, and a prescaler that
    /// keeps the 15-bit counter top in range.
    #[test]
    fn the_config_files_carry_the_modules_settings() {
        let mut mcu = everything();
        for m in &mut mcu.modules {
            use crate::panels::mcu_module::modules::{ModuleConfig, Parity};
            match &mut m.config {
                ModuleConfig::Usart(c) => {
                    c.baud_rate = 9_600;
                    c.parity = Parity::Even;
                }
                ModuleConfig::Spi(c) => {
                    c.clock_hz = 3_000_000;
                    c.mode = 3;
                }
                ModuleConfig::I2c(c) => c.clock_hz = 100_000,
                _ => {}
            }
        }
        let files = mcu.config_files();
        let body = |name: &str| {
            files
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, b)| b.as_str())
                .unwrap_or_else(|| panic!("no {name}"))
        };

        let uarte = body("uarte0.rs");
        assert!(uarte.contains("Baudrate::BAUD9600;"), "{uarte}");
        assert!(uarte.contains("Parity::INCLUDED;"), "{uarte}");
        assert!(
            uarte.contains("uarte::Pins { rxd, txd, cts, rts }"),
            "{uarte}"
        );

        let spim = body("spim2.rs");
        assert!(spim.contains("// 3000000 Hz asked for"), "{spim}");
        assert!(spim.contains("Frequency::M2;"), "{spim}");
        assert!(spim.contains("MODE_3;"), "{spim}");
        // And it is in the regenerated half, where the module can change it.
        let e = spim.find("// <<< GENERATED END >>>").unwrap();
        assert!(spim[..e].contains("pub const MODE"), "{spim}");

        let twim = body("twim0/mod.rs");
        assert!(twim.contains("Frequency::K100;"), "{twim}");
        assert!(!twim.contains("asked for"), "{twim}");

        // 50 Hz: 16 MHz / 50 = 320 000, far over 32 767, so the divider climbs
        // until the top fits: /16 gives 20 000.
        let pwm = body("pwm0.rs");
        assert!(pwm.contains("pub const FREQ_HZ: u32 = 50;"), "{pwm}");
        assert!(pwm.contains("Prescaler::Div16;"), "{pwm}");
        assert!(
            pwm.contains("pub const DUTY_C0_X100: u32 = 750; // 7.5 %"),
            "{pwm}"
        );
        assert!(
            pwm.contains("pub const DUTY_C1_X100: u32 = 1000; // 10 %"),
            "{pwm}"
        );
        assert!(
            pwm.contains("pwm.set_output_pin(Channel::C0, c0);"),
            "{pwm}"
        );
        assert!(
            pwm.contains("fn set_duty_pwm_0_c1(&mut self, value: u32)"),
            "{pwm}"
        );
    }

    #[test]
    fn the_prescaler_is_the_smallest_that_fits() {
        assert_eq!(super::pwm_prescaler(20_000), (1, "Div1"));
        assert_eq!(super::pwm_prescaler(1_000), (1, "Div1"));
        assert_eq!(super::pwm_prescaler(400), (2, "Div2"));
        assert_eq!(super::pwm_prescaler(50), (16, "Div16"));
        assert_eq!(super::pwm_prescaler(1), (128, "Div128"));
        assert_eq!(super::pwm_actual_hz(50, 16), 50);
        assert_eq!(super::pwm_actual_hz(1_000, 1), 1_000);
        // 8 MHz / 300 = 26 666 rem 200: the top rounds down, and the integer
        // division on the way back lands on the number asked for anyway.
        assert_eq!(super::pwm_actual_hz(300, 2), 300);
        // 16 MHz / 128 = 125 kHz; at 1 Hz the top would be 125 000, which the
        // 15-bit counter clamps to 32 767, so the pad runs at 3 Hz instead.
        assert_eq!(super::pwm_actual_hz(1, 128), 3);
    }

    #[test]
    fn the_fixed_rates_round_the_way_the_comments_say() {
        assert_eq!(super::baud_variant(115_200).1, "BAUD115200");
        assert_eq!(super::baud_variant(110_000).1, "BAUD115200");
        assert_eq!(super::baud_variant(1_000_000).1, "BAUD1M");
        assert_eq!(super::spim_frequency(8_000_000).1, "M8");
        assert_eq!(super::spim_frequency(16_000_000).1, "M8");
        assert_eq!(super::spim_frequency(1).1, "K125");
        assert_eq!(super::twim_frequency(400_000).1, "K400");
        assert_eq!(super::twim_frequency(399_999).1, "K250");
        assert_eq!(super::twim_frequency(0).1, "K100");
    }

    /// Re-generating must replace the marked block and keep everything else.
    #[test]
    fn regeneration_keeps_the_users_code() {
        let mut mcu = microbit(&[("P0.21", PinFunction::GpioOutput)]);
        let first = mcu.fresh_main_rs();
        let edited = first
            .replace(
                "use panic_halt as _;",
                "use panic_halt as _;\nuse my_crate::Thing;",
            )
            .replace(
                "        // Your main loop code here.",
                "        p0_21_out.set_high().unwrap();\n        my_own_helper();",
            );
        for p in mcu.iter_all_pins_mut() {
            if p.name.starts_with("P0.14") {
                p.selected_function = PinFunction::GpioInput;
            }
        }
        let again = mcu.update_main_rs(&edited);
        assert!(again.contains("use my_crate::Thing;"), "{again}");
        assert!(again.contains("my_own_helper();"), "{again}");
        assert!(
            again.contains("port0.p0_14.into_floating_input()"),
            "{again}"
        );
        assert!(
            again.contains("port0.p0_21.into_push_pull_output"),
            "{again}"
        );
        assert_eq!(again.matches("#[cortex_m_rt::entry]").count(), 1, "{again}");
    }

    /// The editable half of every config file sits OUTSIDE the markers.
    #[test]
    fn only_the_constants_are_regenerated() {
        const BEGIN: &str = "// <<< GENERATED>>>";
        const END: &str = "// <<< GENERATED END >>>";
        let files = everything().config_files();
        assert_eq!(files.len(), 4);
        for (name, body) in &files {
            assert_eq!(body.matches(BEGIN).count(), 1, "{name}");
            assert_eq!(body.matches(END).count(), 1, "{name}");
            let b = body.find(BEGIN).unwrap();
            let e = body.find(END).unwrap();
            assert!(b < e, "{name}");
            let inside = &body[b..e];
            let outside = &body[e..];
            for forbidden in ["pub fn init", "pub trait", "pub type Handle"] {
                assert!(
                    !inside.contains(forbidden),
                    "{name}: {forbidden} inside:\n{body}"
                );
            }
            assert!(outside.contains("pub fn init"), "{name}");
            assert!(outside.contains("pub type Handle"), "{name}");
            assert!(inside.contains("const "), "{name}");
        }
    }

    /// The other branches: crystal HFCLK, synthesized LFCLK, open-drain and
    /// pull-down pins, flow control on the UARTE, SPI mode 3 on a bus with only
    /// SCK, and a PWM with no frequency set.
    fn the_other_branches() -> Mcu {
        let mut mcu = microbit(&[
            ("P0.21", PinFunction::GpioOutput),
            ("P0.14", PinFunction::GpioInput),
            ("P0.06", PinFunction::UsartTx(0)),
            ("P1.08", PinFunction::UsartRx(0)),
            ("P0.02", PinFunction::UsartCts(0)),
            ("P0.03", PinFunction::UsartRts(0)),
            ("P0.17", PinFunction::SpiSck(2)),
            (
                "P0.00",
                PinFunction::TimerPwm {
                    timer: 1,
                    channel: 2,
                },
            ),
        ]);
        for p in mcu.iter_all_pins_mut() {
            match p.name.split_whitespace().next() {
                Some("P0.21") => p.io_mode = Some(GpioMode::OpenDrain),
                Some("P0.14") => p.io_mode = Some(GpioMode::PullDown),
                _ => {}
            }
        }
        select(&mut mcu, "hfclk_src", 1);
        select(&mut mcu, "lfclk_src", 1);
        mcu.reconcile_modules();
        for m in &mut mcu.modules {
            if let crate::panels::mcu_module::modules::ModuleConfig::Spi(c) = &mut m.config {
                c.mode = 3;
            }
        }
        mcu
    }

    /// Two micro:bit projects on disk, for a real cross-compile.
    ///
    /// Every HAL call above was read from the nrf-hal-common 0.19 source, and
    /// the compiler is still the only thing that can say the reading was
    /// right: type-state on `Clocks`, `degrade()` on every bus pin, the SAADC
    /// on the typed one. Two projects rather than one because a branch the
    /// emitted project never takes is a branch the compiler never sees.
    ///
    /// ```text
    /// cargo test --bin rust_on_chip emit_nrf_project -- --ignored --nocapture
    /// cd %TEMP%\eide_nrf52833_check && cargo check --target thumbv7em-none-eabihf
    /// ```
    #[test]
    #[ignore = "writes projects to disk for a manual cross-compile"]
    fn emit_nrf_project() {
        // The watchdog at both ends of the tab's range: the longest period
        // (1.4e8 ticks in the CRV) on one project, the floor - exactly the
        // 15-tick minimum - on the other.
        let floor = crate::panels::mcu_module::watchdog::nrf_range_us().0;
        for (mut mcu, dir_name, timeout_us) in [
            (everything(), "eide_nrf52833_check", u32::MAX),
            (the_other_branches(), "eide_nrf52833_alt_check", floor),
        ] {
            mcu.watchdog.nrf =
                Some(crate::panels::mcu_module::watchdog::NrfWdtConfig { timeout_us });
            assert!(
                mcu.fresh_main_rs()
                    .contains("let watchdog = pins::configs::watchdog::init(p.WDT);"),
                "{dir_name}: no watchdog in main.rs"
            );
            // A device on TWIM0 - added here rather than in `everything()`,
            // which unit tests share - so `twim0/` is a folder with a device.
            // The other branches wire no TWIM.
            if dir_name == "eide_nrf52833_check" {
                assert!(mcu.with_i2c_devices(&[("accel", 0x19)]));
            }
            emit(&mcu, dir_name);
        }
    }

    fn emit(mcu: &Mcu, dir_name: &str) {
        let def = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "nrf52833_microbit_v2")
            .expect("built-in micro:bit v2");
        let main_rs = mcu.fresh_main_rs();
        let files = project_gen::build_project_files(&def.project, &def.toolchain, &main_rs);
        let user: Vec<(String, String)> = mcu.pin_tree_files();
        let dir = std::env::temp_dir().join(dir_name);
        project_gen::clear_project_dir_keep_target(&dir);
        project_gen::write_project(&dir, &files, &user, &mcu.mcu_config_text(), "")
            .expect("write nrf project");
        println!("wrote {}", dir.display());
        println!("target: {}", def.project.target);
    }
}

#[cfg(test)]
mod async_codegen {
    use super::blocking_codegen::{everything, microbit, select};
    use crate::panels::mcu_module::codegen::common::{ASYNC_USER_TAIL, USER_TAIL};
    use crate::panels::mcu_module::mcu::Mcu;
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::modules::{
        ModuleConfig, PwmChannelConfig, PwmCounting, PwmMode, PwmOutput, PwmPolarity, SpiBitOrder,
    };
    use crate::panels::mcu_module::pins::PinFunction;
    use crate::panels::mcu_module::pins::logic::pin::GpioMode;
    use crate::panels::mcu_module::pins::logic::pin::model::Edge;
    use crate::panels::mcu_module::{builtins, project_gen};

    fn on_async(mut mcu: Mcu) -> Mcu {
        mcu.runtime = Runtime::Async;
        assert!(mcu.is_async(), "the micro:bit has an async backend");
        mcu
    }

    /// One of everything on embassy-nrf: each bus on its own vector, the pins
    /// straight out of `Peripherals`, and no config files.
    #[test]
    fn a_wired_board_generates_every_peripheral_on_embassy_nrf() {
        let mcu = on_async(everything());
        let main = mcu.fresh_main_rs();
        for want in [
            "// MCU: BBC micro:bit v2 | HAL: embassy-nrf (async)",
            "embassy_nrf::bind_interrupts!(struct Irqs {",
            "    UARTE0 => embassy_nrf::uarte::InterruptHandler<embassy_nrf::peripherals::UARTE0>;",
            "    SPI2 => embassy_nrf::spim::InterruptHandler<embassy_nrf::peripherals::SPI2>;",
            "    TWISPI0 => embassy_nrf::twim::InterruptHandler<embassy_nrf::peripherals::TWISPI0>;",
            "    SAADC => embassy_nrf::saadc::InterruptHandler;",
            "#[embassy_executor::main]",
            "async fn main(_spawner: embassy_executor::Spawner) {",
            "use embassy_nrf::gpio::{Input, Level, Output, OutputDrive, Pull};",
            "let p = embassy_nrf::init(config);",
            "let mut p0_21_out = Output::new(p.P0_21, Level::Low, OutputDrive::Standard);",
            "let p0_14_in = Input::new(p.P0_14, Pull::None);",
            "embassy_nrf::uarte::Uarte::new(\n        p.UARTE0,\n        p.P1_08, // RXD\n        p.P0_06, // TXD\n        Irqs,\n        uarte0_cfg,\n    );",
            "uarte0_cfg.baudrate = embassy_nrf::uarte::Baudrate::Baud115200;",
            "uarte0_cfg.parity = embassy_nrf::uarte::Parity::Excluded;",
            "embassy_nrf::spim::Spim::new(\n        p.SPI2,\n        Irqs,\n        p.P0_17, // SCK\n        p.P0_01, // MISO\n        p.P0_13, // MOSI\n        spim2_cfg,\n    );",
            "spim2_cfg.frequency = embassy_nrf::spim::Frequency::M1;",
            "spim2_cfg.mode = embassy_nrf::spim::MODE_0;",
            "spim2_cfg.bit_order = embassy_nrf::spim::BitOrder::MsbFirst;",
            "embassy_nrf::twim::Twim::new(\n        p.TWISPI0,\n        Irqs,\n        p.P0_16, // SDA\n        p.P0_08, // SCL\n        twim0_cfg,\n        TWIM0_RAM.init([0; 32]),\n    );",
            "static TWIM0_RAM: static_cell::StaticCell<[u8; 32]> = static_cell::StaticCell::new();",
            "embassy_nrf::pwm::SimplePwm::new_2ch(\n        p.PWM0,\n        p.P0_00, // channel 0\n        p.P1_02, // channel 1\n        &pwm0_cfg,\n    );",
            "embassy_nrf::saadc::ChannelConfig::single_ended(p.P0_02),",
        ] {
            assert!(main.contains(want), "missing {want:?} in:\n{main}");
        }
        assert!(main.ends_with(ASYNC_USER_TAIL), "{main}");
        for gone in [
            "nrf52833_hal",
            "pins::configs::",
            "cortex_m_rt::entry",
            "degrade()",
        ] {
            assert!(!main.contains(gone), "{gone} in:\n{main}");
        }
        assert!(mcu.config_files().is_empty(), "no config files on Async");
    }

    /// `Config` gets the two muxes, and the names are embassy-nrf's.
    #[test]
    fn the_clock_config_follows_the_two_muxes() {
        let main = on_async(microbit(&[])).fresh_main_rs();
        assert!(
            main.contains("config.hfclk_source = embassy_nrf::config::HfclkSource::Internal;"),
            "{main}"
        );
        assert!(
            main.contains("config.lfclk_source = embassy_nrf::config::LfclkSource::InternalRC;"),
            "{main}"
        );
        // Nothing wired: no interrupts to bind and no `Irqs` to name.
        assert!(!main.contains("bind_interrupts"), "{main}");

        let mut mcu = on_async(microbit(&[]));
        select(&mut mcu, "hfclk_src", 1);
        select(&mut mcu, "lfclk_src", 1);
        let main = mcu.fresh_main_rs();
        assert!(main.contains("HfclkSource::ExternalXtal;"), "{main}");
        assert!(main.contains("LfclkSource::Synthesized;"), "{main}");
    }

    /// An armed input is a task that owns the pin; a plain one is a binding.
    /// The spawner is named only when something is spawned.
    #[test]
    fn an_armed_input_becomes_a_task_that_owns_it() {
        let mut mcu = on_async(microbit(&[
            ("P0.14", PinFunction::GpioInput),
            ("P0.23", PinFunction::GpioInput),
        ]));
        for p in mcu.iter_all_pins_mut() {
            if p.name.starts_with("P0.14") {
                p.irq = Some(Edge::Falling);
                p.io_mode = Some(GpioMode::PullUp);
            }
        }
        let main = mcu.fresh_main_rs();
        for want in [
            "#[embassy_executor::task]\nasync fn p0_14_in_irq(mut pin: embassy_nrf::gpio::Input<'static>) {",
            // The body is a call to the user's hook, not a comment to fill in.
            "pin.wait_for_falling_edge().await;\n        on_p0_14_in_edge(pin.is_high()).await;",
            "let p0_14_in = Input::new(p.P0_14, Pull::Up);\n    spawner.spawn(p0_14_in_irq(p0_14_in).unwrap());",
            "async fn main(spawner: embassy_executor::Spawner) {",
            "let p0_23_in = Input::new(p.P0_23, Pull::None);",
        ] {
            assert!(main.contains(want), "missing {want:?} in:\n{main}");
        }
        assert!(!main.contains("p0_23_in_irq"), "{main}");
        // The task sits above `main`, outside it.
        assert!(main.find("async fn p0_14_in_irq") < main.find("async fn main("));
    }

    /// Pull and drive from the pin panel, and the same default the panel shows.
    #[test]
    fn the_io_mode_picks_pull_and_drive() {
        let mut mcu = on_async(microbit(&[
            ("P0.21", PinFunction::GpioOutput),
            ("P0.23", PinFunction::GpioInput),
        ]));
        for p in mcu.iter_all_pins_mut() {
            match p.name.split_whitespace().next() {
                Some("P0.21") => p.io_mode = Some(GpioMode::OpenDrain),
                Some("P0.23") => p.io_mode = Some(GpioMode::PullDown),
                _ => {}
            }
        }
        let main = mcu.fresh_main_rs();
        assert!(
            main.contains("Output::new(p.P0_21, Level::High, OutputDrive::Standard0Disconnect1)"),
            "{main}"
        );
        assert!(main.contains("Input::new(p.P0_23, Pull::Down)"), "{main}");

        use crate::panels::mcu_module::codegen::family::gpio_modes_for;
        assert_eq!(
            gpio_modes_for(&mcu, &PinFunction::GpioInput),
            &[GpioMode::Floating, GpioMode::PullUp, GpioMode::PullDown]
        );
        assert_eq!(
            gpio_modes_for(&mcu, &PinFunction::GpioOutput),
            &[GpioMode::PushPull, GpioMode::OpenDrain]
        );
    }

    /// embassy-nrf builds a SPIM from either data line alone, and from neither
    /// it builds nothing - which then binds no interrupt.
    #[test]
    fn the_spim_constructor_follows_the_data_lines() {
        let tx = on_async(microbit(&[
            ("P0.17", PinFunction::SpiSck(2)),
            ("P0.13", PinFunction::SpiMosi(2)),
        ]))
        .fresh_main_rs();
        assert!(
            tx.contains("Spim::new_txonly(\n        p.SPI2,\n        Irqs,\n        p.P0_17, // SCK\n        p.P0_13, // MOSI\n"),
            "{tx}"
        );

        let rx = on_async(microbit(&[
            ("P0.17", PinFunction::SpiSck(2)),
            ("P0.01", PinFunction::SpiMiso(2)),
        ]))
        .fresh_main_rs();
        assert!(
            rx.contains("Spim::new_rxonly(\n        p.SPI2,\n        Irqs,\n        p.P0_17, // SCK\n        p.P0_01, // MISO\n"),
            "{rx}"
        );

        let sck = on_async(microbit(&[("P0.17", PinFunction::SpiSck(2))])).fresh_main_rs();
        assert!(sck.contains("// SPIM2: only SCK is wired."), "{sck}");
        assert!(!sck.contains("Spim::"), "{sck}");
        assert!(!sck.contains("bind_interrupts"), "{sck}");
    }

    /// Mode and bit order come from the module.
    #[test]
    fn the_spim_config_carries_the_modules_settings() {
        let mut mcu = on_async(everything());
        for m in &mut mcu.modules {
            if let ModuleConfig::Spi(c) = &mut m.config {
                c.mode = 3;
                c.bit_order = SpiBitOrder::LsbFirst;
                c.clock_hz = 3_000_000;
            }
        }
        let main = mcu.fresh_main_rs();
        assert!(
            main.contains("spim2_cfg.mode = embassy_nrf::spim::MODE_3;"),
            "{main}"
        );
        assert!(
            main.contains("spim2_cfg.bit_order = embassy_nrf::spim::BitOrder::LsbFirst;"),
            "{main}"
        );
        assert!(main.contains("// 3000000 Hz asked for"), "{main}");
        assert!(main.contains("Frequency::M2;"), "{main}");
    }

    /// SPIM0 and TWIM0 are one block: the first built keeps it, the second is a
    /// comment, and the vector is bound once. The TWIM that lost writes no
    /// buffer, so the manifest does not get `static_cell` for it either.
    #[test]
    fn a_shared_block_is_built_once() {
        let mcu = on_async(microbit(&[
            ("P0.17", PinFunction::SpiSck(0)),
            ("P0.13", PinFunction::SpiMosi(0)),
            ("P0.08", PinFunction::I2cScl(0)),
            ("P0.16", PinFunction::I2cSda(0)),
        ]));
        let main = mcu.fresh_main_rs();
        assert!(
            main.contains("Spim::new_txonly(\n        p.TWISPI0,"),
            "{main}"
        );
        assert!(
            main.contains(
                "// TWIM0 is not built: it is the same block as SPIM0 (TWISPI0), which has it."
            ),
            "{main}"
        );
        assert!(!main.contains("Twim::new"), "{main}");
        assert_eq!(main.matches("    TWISPI0 =>").count(), 1, "{main}");
        assert!(!super::needs_static_cell(&mcu));
    }

    /// `needs_static_cell` and the generated code agree, on every shape.
    #[test]
    fn static_cell_follows_the_code() {
        let spi_only = microbit(&[
            ("P0.17", PinFunction::SpiSck(2)),
            ("P0.13", PinFunction::SpiMosi(2)),
        ]);
        for (mcu, want) in [
            (on_async(everything()), true),
            (on_async(spi_only), false),
            // Blocking writes no StaticCell, whatever is wired.
            (everything(), false),
        ] {
            let main = mcu.fresh_main_rs();
            assert_eq!(super::needs_static_cell(&mcu), want, "{main}");
            assert_eq!(main.contains("static_cell::"), want, "{main}");
        }
    }

    /// Flow control is the CTS+RTS pair; one of them alone says so.
    #[test]
    fn flow_control_takes_the_pair() {
        let both = on_async(microbit(&[
            ("P0.06", PinFunction::UsartTx(0)),
            ("P1.08", PinFunction::UsartRx(0)),
            ("P0.02", PinFunction::UsartCts(0)),
            ("P0.03", PinFunction::UsartRts(0)),
        ]))
        .fresh_main_rs();
        assert!(
            both.contains("Uarte::new_with_rtscts(\n        p.UARTE0,\n        p.P1_08, // RXD\n        p.P0_06, // TXD\n        p.P0_02, // CTS\n        p.P0_03, // RTS\n        Irqs,"),
            "{both}"
        );

        let cts = on_async(microbit(&[
            ("P0.06", PinFunction::UsartTx(0)),
            ("P1.08", PinFunction::UsartRx(0)),
            ("P0.02", PinFunction::UsartCts(0)),
        ]))
        .fresh_main_rs();
        assert!(cts.contains("// UARTE0: only CTS is wired."), "{cts}");
        assert!(cts.contains("Uarte::new(\n        p.UARTE0,"), "{cts}");
        assert!(!cts.contains("new_with_rtscts"), "{cts}");
    }

    /// The baud names are embassy-nrf's, which are not nrf-hal's.
    #[test]
    fn the_baud_is_spelled_the_embassy_way() {
        assert_eq!(super::embassy_baud("BAUD115200"), "Baud115200");
        assert_eq!(super::embassy_baud("BAUD1M"), "Baud1m");
        assert_eq!(
            super::embassy_baud(super::baud_variant(9_600).1),
            "Baud9600"
        );
    }

    /// Counter shape, drive and polarity reach `SimpleConfig` and the duty.
    ///
    /// `DutyCycle::inverted(v)` is high for v / top of the period, so an
    /// active-high channel uses it; active low, or mode 2, uses `normal`, and
    /// both together cancel back to `inverted`.
    #[test]
    fn the_pwm_shape_reaches_the_config_and_the_duty() {
        let mut mcu = on_async(everything());
        for m in &mut mcu.modules {
            if let ModuleConfig::Timer(c) = &mut m.config {
                c.counting = PwmCounting::CenterUpInterrupts;
                c.set_channel(
                    0,
                    PwmChannelConfig {
                        output: PwmOutput::OpenDrain,
                        polarity: PwmPolarity::ActiveLow,
                        mode: PwmMode::Mode1,
                    },
                );
                c.set_channel(
                    1,
                    PwmChannelConfig {
                        output: PwmOutput::PushPull,
                        polarity: PwmPolarity::ActiveLow,
                        mode: PwmMode::Mode2,
                    },
                );
            }
        }
        let main = mcu.fresh_main_rs();
        for want in [
            "pwm0_cfg.counter_mode = embassy_nrf::pwm::CounterMode::UpAndDown;",
            "pwm0_cfg.ch0_drive = embassy_nrf::gpio::OutputDrive::Standard0Disconnect1;",
            // 50 Hz center-aligned: 100 counts a second, 16 MHz / 8 / 100.
            "pwm0_cfg.prescaler = embassy_nrf::pwm::Prescaler::Div8;",
            "pwm0_cfg.max_duty = 20000;",
            "// Channel 0 on P0.00: 7.5 % of the period low.",
            "pwm0.set_duty(0, embassy_nrf::pwm::DutyCycle::normal((pwm0.max_duty() as u32 * 750 / 10_000) as u16));",
            "// Channel 1 on P1.02: 10 % of the period high.",
            "pwm0.set_duty(1, embassy_nrf::pwm::DutyCycle::inverted((pwm0.max_duty() as u32 * 1000 / 10_000) as u16));",
        ] {
            assert!(main.contains(want), "missing {want:?} in:\n{main}");
        }
        assert!(!main.contains("ch1_drive"), "{main}");
    }

    /// A lone channel 2 is output 0 of `SimplePwm`, and the comment says so.
    #[test]
    fn a_pwm_channel_that_is_not_its_slot_is_named() {
        let mcu = on_async(microbit(&[(
            "P0.00",
            PinFunction::TimerPwm {
                timer: 1,
                channel: 2,
            },
        )]));
        let main = mcu.fresh_main_rs();
        assert!(
            main.contains("SimplePwm::new_1ch(\n        p.PWM1,\n        p.P0_00, // channel 2\n"),
            "{main}"
        );
        assert!(
            main.contains("// Channel 2 on P0.00, output 0 here: 0 % of the period high."),
            "{main}"
        );
        // Nothing to write into the config, so no `mut` for rustc to flag, and
        // a 0 % duty that is not `x * 0`.
        assert!(main.contains("embassy-nrf's default stands"), "{main}");
        assert!(
            main.contains("    let pwm1_cfg = embassy_nrf::pwm::SimpleConfig::default();"),
            "{main}"
        );
        assert!(
            main.contains("pwm1.set_duty(0, embassy_nrf::pwm::DutyCycle::inverted(0));"),
            "{main}"
        );
        assert!(!main.contains("* 0 /"), "{main}");
    }

    /// A full duty is the top itself.
    #[test]
    fn a_full_duty_is_the_top() {
        let mut mcu = on_async(everything());
        for m in &mut mcu.modules {
            if let ModuleConfig::Timer(c) = &mut m.config {
                c.set_duty_x100(0, 10_000);
            }
        }
        let main = mcu.fresh_main_rs();
        assert!(
            main.contains(
                "pwm0.set_duty(0, embassy_nrf::pwm::DutyCycle::inverted(pwm0.max_duty()));"
            ),
            "{main}"
        );
    }

    #[test]
    fn center_aligned_timing_halves_the_top() {
        // Edge: the blocking backend's numbers.
        assert_eq!(super::pwm_timing(50, false), (16, "Div16", 20_000, 50));
        assert_eq!(super::pwm_timing(1_000, false), (1, "Div1", 16_000, 1_000));
        // Center: twice the counts for the same frequency.
        assert_eq!(super::pwm_timing(50, true), (8, "Div8", 20_000, 50));
        assert_eq!(super::pwm_timing(1_000, true), (1, "Div1", 8_000, 1_000));
    }

    /// On Async the NFC pads need no hand work, and the note says why rather
    /// than giving the NVMC recipe.
    #[test]
    fn the_nfc_note_names_the_cargo_feature() {
        let main = on_async(microbit(&[("P0.10", PinFunction::GpioInput)])).fresh_main_rs();
        assert!(
            main.contains("// P0.10 is one of the NFC antenna pins."),
            "{main}"
        );
        assert!(main.contains("`nfc-pins-as-gpio`"), "{main}");
        assert!(!main.contains("NVMC"), "{main}");
        assert!(main.contains("Input::new(p.P0_10, Pull::None)"), "{main}");
    }

    /// A runtime switch keeps the header, so the header must be the same file
    /// on both sides: a pristine project switched either way is exactly the
    /// project generated fresh on the new runtime.
    #[test]
    fn a_runtime_switch_lands_on_the_fresh_file() {
        let blocking = everything();
        let asynchronous = on_async(everything());

        let switched = asynchronous.update_main_rs(&blocking.fresh_main_rs());
        assert_eq!(switched, asynchronous.fresh_main_rs());
        let back = blocking.update_main_rs(&switched);
        assert_eq!(back, blocking.fresh_main_rs());
    }

    /// With the user's own edits, the switch keeps them and still refreshes
    /// the provenance line.
    #[test]
    fn a_runtime_switch_keeps_the_users_code() {
        let blocking = microbit(&[("P0.21", PinFunction::GpioOutput)]);
        let edited = blocking
            .fresh_main_rs()
            .replace(
                "use panic_halt as _;",
                "use panic_halt as _;\nuse my_crate::Thing;",
            )
            .replace(
                "        // Your main loop code here.",
                "        my_own_helper();",
            );
        let asynchronous = on_async(microbit(&[("P0.21", PinFunction::GpioOutput)]));
        let switched = asynchronous.update_main_rs(&edited);
        assert!(switched.contains("use my_crate::Thing;"), "{switched}");
        assert!(switched.contains("my_own_helper();"), "{switched}");
        assert!(switched.contains("HAL: embassy-nrf (async)"), "{switched}");
        assert!(!switched.contains("(blocking)"), "{switched}");
        assert_eq!(switched.matches("// MCU: ").count(), 1, "{switched}");
        // An edited tail is the user's: the blocking loop stays as they left it.
        assert!(!switched.contains(ASYNC_USER_TAIL), "{switched}");
        assert!(switched.contains("embassy_nrf::init(config)"), "{switched}");
        assert!(!switched.contains("cortex_m_rt::entry"), "{switched}");

        // And a pristine blocking tail becomes the async one.
        let pristine = asynchronous.update_main_rs(&blocking.fresh_main_rs());
        assert!(pristine.ends_with(ASYNC_USER_TAIL), "{pristine}");
        assert!(!pristine.contains(USER_TAIL), "{pristine}");
    }

    /// The other async branches: crystal HFCLK, synthesized LFCLK, an armed
    /// pull-up input, open-drain, CTS/RTS on the UARTE, a TX-only SPIM in mode
    /// 3 LSB first, TWIM1, a center-aligned active-low open-drain PWM on a
    /// channel that is not its slot, and a PWM block with no frequency.
    fn the_other_async_branches() -> Mcu {
        let mut mcu = on_async(microbit(&[
            ("P0.21", PinFunction::GpioOutput),
            ("P0.14", PinFunction::GpioInput),
            ("P0.23", PinFunction::GpioInput),
            ("P0.06", PinFunction::UsartTx(0)),
            ("P1.08", PinFunction::UsartRx(0)),
            ("P0.02", PinFunction::UsartCts(0)),
            ("P0.03", PinFunction::UsartRts(0)),
            ("P0.17", PinFunction::SpiSck(2)),
            ("P0.13", PinFunction::SpiMosi(2)),
            ("P0.08", PinFunction::I2cScl(1)),
            ("P0.16", PinFunction::I2cSda(1)),
            (
                "P0.00",
                PinFunction::TimerPwm {
                    timer: 1,
                    channel: 2,
                },
            ),
            (
                "P1.02",
                PinFunction::TimerPwm {
                    timer: 2,
                    channel: 0,
                },
            ),
            ("P0.04", PinFunction::AdcChannel { adc: 0, channel: 2 }),
            ("P0.10", PinFunction::GpioOutput),
        ]));
        for p in mcu.iter_all_pins_mut() {
            match p.name.split_whitespace().next() {
                Some("P0.21") => p.io_mode = Some(GpioMode::OpenDrain),
                Some("P0.14") => {
                    p.io_mode = Some(GpioMode::PullUp);
                    p.irq = Some(Edge::Falling);
                }
                Some("P0.23") => {
                    p.io_mode = Some(GpioMode::PullDown);
                    p.irq = Some(Edge::Both);
                }
                _ => {}
            }
        }
        select(&mut mcu, "hfclk_src", 1);
        select(&mut mcu, "lfclk_src", 1);
        mcu.reconcile_modules();
        for m in &mut mcu.modules {
            match &mut m.config {
                ModuleConfig::Spi(c) => {
                    c.mode = 3;
                    c.bit_order = SpiBitOrder::LsbFirst;
                }
                ModuleConfig::Usart(c) => c.baud_rate = 9_600,
                // A module's default is 1 kHz, so "no frequency" has to be set.
                ModuleConfig::Timer(c) if c.instance == 2 => c.freq_hz = 0,
                ModuleConfig::Timer(c) if c.instance == 1 => {
                    c.freq_hz = 1_000;
                    c.counting = PwmCounting::CenterBothInterrupts;
                    c.set_duty_x100(2, 2_500);
                    c.set_channel(
                        2,
                        PwmChannelConfig {
                            output: PwmOutput::OpenDrain,
                            polarity: PwmPolarity::ActiveLow,
                            mode: PwmMode::Mode1,
                        },
                    );
                }
                _ => {}
            }
        }
        mcu
    }

    /// Two micro:bit projects on embassy-nrf, for a real cross-compile.
    ///
    /// Written the way the application writes them, not the way this file
    /// would like to: both `main.rs` and `Cargo.toml` come through a runtime
    /// SWITCH from the blocking project, which is the path a user takes.
    /// `main.rs` only builds that way if the shared header is right, and the
    /// manifest only if `refresh_hal_dependency` swaps nrf52833-hal for
    /// embassy-nrf before the same `ensure_*` calls `app.rs` makes.
    ///
    /// ```text
    /// cargo test --bin rust_on_chip emit_nrf_async_project -- --ignored --nocapture
    /// cd %TEMP%\eide_nrf52833_async_check && cargo check --target thumbv7em-none-eabihf
    /// ```
    #[test]
    #[ignore = "writes projects to disk for a manual cross-compile"]
    fn emit_nrf_async_project() {
        for (mut mcu, dir_name) in [
            (on_async(everything()), "eide_nrf52833_async_check"),
            (the_other_async_branches(), "eide_nrf52833_async_alt_check"),
        ] {
            // The watchdog rides along, set BEFORE the blocking clone below, so
            // the runtime switch carries it the way a user's project would.
            mcu.watchdog.nrf =
                Some(crate::panels::mcu_module::watchdog::NrfWdtConfig::default_for());
            // Two devices on the TWIM that has one: no config files on this
            // runtime, so their addresses are consts in main.rs - built from
            // the names the device files had before buses were folders.
            if dir_name == "eide_nrf52833_async_check" {
                assert!(mcu.with_i2c_devices(&[("accel", 0x19), ("", 0x1E)]));
            }
            let def = builtins::builtin_definitions()
                .into_iter()
                .find(|d| d.id == "nrf52833_microbit_v2")
                .expect("built-in micro:bit v2");
            let mut blocking = mcu.clone();
            blocking.runtime = Runtime::Blocking;
            let main_rs = mcu.update_main_rs(&blocking.fresh_main_rs());
            assert_eq!(
                main_rs,
                mcu.fresh_main_rs(),
                "the switch lands on the fresh file"
            );

            let project = crate::panels::mcu_module::mcu_def::build_cfg(&def, Some(&mcu));
            assert!(
                project.hal_dep.starts_with("embassy-nrf"),
                "{}",
                project.hal_dep
            );
            let files = project_gen::build_project_files(&project, &def.toolchain, &main_rs);
            // The manifest as the blocking project had it, then switched. It
            // lands byte for byte on the fresh one, so writing `files` below
            // writes the switched manifest.
            let blocking_cfg = crate::panels::mcu_module::mcu_def::build_cfg(&def, Some(&blocking));
            let blocking_toml =
                project_gen::build_project_files(&blocking_cfg, &def.toolchain, "").cargo_toml;
            assert!(blocking_toml.contains("nrf52833-hal"), "{blocking_toml}");
            assert_eq!(
                project_gen::refresh_hal_dependency(&blocking_toml, &project, &def.toolchain),
                files.cargo_toml,
                "the switch lands on the fresh manifest"
            );
            // What the app writes: every config file `config_files` returns.
            // This asserted there were none and wrote an empty `configs/mod.rs` -
            // true until the watchdog became this backend's first config file.
            let configs = mcu.config_files();
            let names: Vec<&str> = configs.iter().map(|(n, _)| n.as_str()).collect();
            assert_eq!(names, ["watchdog.rs"], "{dir_name}");
            let user: Vec<(String, String)> = mcu.pin_tree_files();
            let dir = std::env::temp_dir().join(dir_name);
            project_gen::clear_project_dir_keep_target(&dir);
            project_gen::write_project(&dir, &files, &user, &mcu.mcu_config_text(), "")
                .expect("write nrf async project");

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
            // The app's static_cell decision, verbatim.
            let toml = project_gen::ensure_task_priority_deps(
                &toml,
                main_rs.contains("InterruptExecutor") || super::needs_static_cell(&mcu),
                &sources,
            );
            let toml = project_gen::ensure_m0_atomics(&toml, true, &project.target, &sources);
            assert!(
                toml.contains("embedded-hal"),
                "the shared header's import:\n{toml}"
            );
            std::fs::write(&toml_path, toml).expect("write Cargo.toml");
            println!("wrote {}", dir.display());
            println!("target: {}", def.project.target);
        }
    }
}

#[cfg(test)]
mod pin_restore {
    use super::blocking_codegen::{everything, microbit};
    use crate::panels::mcu_module::mcu::{Mcu, Runtime};
    use crate::panels::mcu_module::mcu_config;
    use crate::panels::mcu_module::pins::PinFunction;
    use crate::panels::mcu_module::pins::logic::pin::GpioMode;

    /// Everything about a pad that reaches the generated code.
    fn wiring(mcu: &Mcu) -> Vec<(usize, PinFunction, String, Option<GpioMode>)> {
        mcu.iter_all_pins()
            .filter(|p| !p.reserved)
            .map(|p| {
                (
                    p.number,
                    p.selected_function.clone(),
                    p.custom_label.clone(),
                    p.io_mode,
                )
            })
            .collect()
    }

    /// The nRF counterpart of `every_esp_pin_survives_a_generate_parse_round_trip`.
    ///
    /// The nRF backend writes no `// label` on a binding, so `parse_main_rs`
    /// returned nothing for its projects and `apply_saved_pins` wiped the
    /// diagram on every open. The store is `@pins` in `mcu.config` instead.
    /// This walks the open path in `project_io`'s order - config, then the
    /// pins by number, then `@labels` - on both runtimes, with a named pad and
    /// a pulled-up input in the mix since both reach the generated code, and
    /// asks for the wiring back identical and the file regenerated byte for
    /// byte, which is what makes a reopened project write the main.rs it read.
    #[test]
    fn a_wired_board_comes_back_identical_on_reopen() {
        for runtime in [Runtime::Blocking, Runtime::Async] {
            let mut mcu = everything();
            mcu.runtime = runtime;
            for p in mcu.iter_all_pins_mut() {
                if p.name.starts_with("P0.21") {
                    p.custom_label = "Status LED".into();
                }
                if p.name.starts_with("P0.14") {
                    p.io_mode = Some(GpioMode::PullUp);
                }
            }
            let code = mcu.fresh_main_rs();
            let cfg = mcu.mcu_config_text();
            assert!(
                cfg.contains("@pins\n"),
                "{runtime:?}: the section is written\n{cfg}"
            );

            let mut reopened = microbit(&[]);
            reopened.apply_mcu_config(&cfg);
            reopened.apply_saved_pins_by_number(&mcu_config::parse_pins(&cfg));
            reopened.apply_config_pin_labels(&cfg);

            assert_eq!(wiring(&reopened), wiring(&mcu), "{runtime:?}");
            assert_eq!(
                reopened.fresh_main_rs(),
                code,
                "{runtime:?}: reload changed the generated file"
            );
        }
    }
}

/// The body of an armed input's handler is the user's, and it lives below the
/// tail - the task between the markers only calls it.
#[cfg(test)]
mod edge_hook {
    use super::blocking_codegen::microbit;
    use super::{GEN_BEGIN, GEN_END};
    use crate::panels::mcu_module::mcu::{Mcu, Runtime};
    use crate::panels::mcu_module::pins::PinFunction;
    use crate::panels::mcu_module::pins::logic::pin::Edge;

    /// A micro:bit with Button A armed on a falling edge, on Async.
    fn armed() -> Mcu {
        let mut mcu = microbit(&[("P0.14", PinFunction::GpioInput)]);
        mcu.runtime = Runtime::Async;
        assert!(mcu.is_async());
        arm(&mut mcu, Some(Edge::Falling));
        mcu
    }

    fn arm(mcu: &mut Mcu, edge: Option<Edge>) {
        for p in mcu.iter_all_pins_mut() {
            if p.name.starts_with("P0.14") {
                p.irq = edge;
            }
        }
    }

    /// The generated block alone.
    fn block(main: &str) -> &str {
        let begin = main.find(GEN_BEGIN).expect("begin");
        let end = main.find(GEN_END).expect("end");
        &main[begin..end]
    }

    const CALL: &str = "        on_p0_14_in_edge(pin.is_high()).await;\n";
    const SEED_HEAD: &str = "async fn on_p0_14_in_edge(_high: bool) {\n";
    const BODY: &str = "    FLAG.store(true, core::sync::atomic::Ordering::Relaxed);\n";

    /// Fill the seed's body in, the way a user would.
    fn edited(main: &str) -> String {
        let out = main.replacen("    // Your code here.\n", BODY, 1);
        assert_ne!(out, main, "the seed was there to edit:\n{main}");
        out
    }

    /// A fresh file: the task calls the hook, nothing in the block invites an
    /// edit any more, and the seed sits once below the tail with the clippy
    /// allow a Strict project needs.
    #[test]
    fn a_fresh_file_calls_the_hook_and_seeds_it_below_the_tail() {
        let main = armed().fresh_main_rs();
        assert!(block(&main).contains(CALL), "{main}");
        assert!(!block(&main).contains("code here"), "{main}");
        assert_eq!(main.matches(SEED_HEAD).count(), 1, "{main}");
        let seed = main.find(SEED_HEAD).expect("seed");
        assert!(
            seed > main.find("// Your main loop code here.").expect("tail"),
            "{main}"
        );
        assert!(
            main[..seed].ends_with(
                "#[allow(clippy::unused_async)] // drop once the body awaits something\n"
            ),
            "{main}"
        );
        assert!(
            main.contains("/// P0.14 (pad 5, BTN_A) - called from the `p0_14_in_irq` task"),
            "{main}"
        );
    }

    /// The user's body survives every regeneration that used to lose it: a
    /// pin change on the canvas, a rename of the pin, and a change of edge -
    /// none of which renames the hook.
    #[test]
    fn the_body_survives_a_pin_change_a_rename_and_an_edge_change() {
        let mut mcu = armed();
        let mut main = edited(&mcu.fresh_main_rs());

        for (what, change) in [
            (
                "another pin wired",
                Box::new(|m: &mut Mcu| {
                    let led = m
                        .iter_all_pins()
                        .find(|p| p.name.starts_with("P0.21"))
                        .expect("P0.21")
                        .number;
                    m.find_pin_mut(led).expect("pad").selected_function = PinFunction::GpioOutput;
                }) as Box<dyn Fn(&mut Mcu)>,
            ),
            (
                "the pin renamed",
                Box::new(|m: &mut Mcu| {
                    for p in m.iter_all_pins_mut() {
                        if p.name.starts_with("P0.14") {
                            p.custom_label = "Button A".into();
                        }
                    }
                }),
            ),
            (
                "the edge changed",
                Box::new(|m: &mut Mcu| arm(m, Some(Edge::Both))),
            ),
        ] {
            change(&mut mcu);
            main = mcu.update_main_rs(&main);
            assert_eq!(main.matches(BODY).count(), 1, "{what}:\n{main}");
            assert_eq!(main.matches(SEED_HEAD).count(), 1, "{what}:\n{main}");
            assert!(block(&main).contains(CALL), "{what}:\n{main}");
        }
        assert!(
            block(&main).contains("wait_for_any_edge"),
            "the last change took:\n{main}"
        );
    }

    /// Blocking generates no handler, so the hook goes uncalled and stays as
    /// dead code - it names no crate, so it compiles either way. Back on
    /// Async it is called again, with the body intact and no second seed.
    #[test]
    fn a_switch_to_blocking_keeps_the_hook_and_back_to_async_calls_it() {
        let mut mcu = armed();
        let main = edited(&mcu.fresh_main_rs());

        mcu.runtime = Runtime::Blocking;
        let blocking = mcu.update_main_rs(&main);
        assert!(!block(&blocking).contains("on_p0_14_in_edge"), "{blocking}");
        assert_eq!(blocking.matches(SEED_HEAD).count(), 1, "{blocking}");
        assert_eq!(blocking.matches(BODY).count(), 1, "{blocking}");

        mcu.runtime = Runtime::Async;
        let back = mcu.update_main_rs(&blocking);
        assert!(block(&back).contains(CALL), "{back}");
        assert_eq!(back.matches(SEED_HEAD).count(), 1, "{back}");
        assert_eq!(back.matches(BODY).count(), 1, "{back}");
    }

    /// A project saved before this change: the task carries the old inline
    /// comment and there is no hook. One update gives it the call and one
    /// seed - whatever was typed into the block is gone, as it would have been
    /// on the next regeneration anyway.
    #[test]
    fn an_old_project_gets_the_call_and_one_seed() {
        let mcu = armed();
        let fresh = mcu.fresh_main_rs();
        let seed = fresh
            .find("\n/// P0.14 (pad 5, BTN_A) - called")
            .expect("seed");
        let old = fresh[..seed]
            .replacen(CALL, "        // The edge arrived. Your code here.\n", 1)
            .replacen(
                "The task owns the pin;\n/// `on_p0_14_in_edge` below `main` is yours.",
                "The task owns the pin.",
                1,
            );
        assert!(!old.contains("on_p0_14_in_edge"), "{old}");

        let main = mcu.update_main_rs(&old);
        assert!(block(&main).contains(CALL), "{main}");
        assert_eq!(main.matches(SEED_HEAD).count(), 1, "{main}");
    }

    /// A pin that is not armed reports no hook, so nothing is seeded for it.
    #[test]
    fn a_plain_input_seeds_nothing() {
        let mut mcu = armed();
        arm(&mut mcu, None);
        let main = mcu.fresh_main_rs();
        assert!(!main.contains("on_p0_14_in_edge"), "{main}");
    }
}

/// The WDT on both nRF runtimes. The two HALs disagree about the one thing
/// that matters most - whether configuring starts it - so each runtime gets
/// the shape that keeps a freshly generated project from resetting itself.
#[cfg(test)]
mod watchdog_nrf {
    use crate::panels::mcu_module::builtins;
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::watchdog::NrfWdtConfig;

    fn microbit(runtime: Runtime, timeout_us: Option<u32>) -> super::Mcu {
        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "nrf52833_microbit_v2")
            .expect("built-in micro:bit v2")
            .build_mcu();
        mcu.runtime = runtime;
        mcu.watchdog.nrf = timeout_us.map(|timeout_us| NrfWdtConfig { timeout_us });
        mcu
    }

    fn file(mcu: &super::Mcu) -> String {
        mcu.config_files()
            .into_iter()
            .find(|(n, _)| n == "watchdog.rs")
            .map(|(_, b)| b)
            .expect("watchdog.rs")
    }

    #[test]
    fn nothing_is_generated_until_the_watchdog_is_switched_on() {
        for rt in [Runtime::Blocking, Runtime::Async] {
            let mcu = microbit(rt, None);
            assert!(!mcu.fresh_main_rs().contains("watchdog"), "{rt:?}");
            assert!(mcu.config_files().is_empty(), "{rt:?}");
        }
    }

    /// nrf-hal keeps configuring and starting apart, so Blocking configures
    /// in main.rs and leaves `activate` to the user.
    #[test]
    fn blocking_configures_and_leaves_activate_to_the_user() {
        let mcu = microbit(Runtime::Blocking, Some(1_000_000));
        let main = mcu.fresh_main_rs();
        assert!(
            main.contains("let watchdog = pins::configs::watchdog::init(p.WDT);"),
            "{main}"
        );
        assert!(
            !main.contains(".activate"),
            "never started for the user:\n{main}"
        );
        let body = file(&mcu);
        assert!(body.contains("const TIMEOUT_TICKS: u32 = 32768;"), "{body}");
        assert!(
            body.contains("use nrf52833_hal::wdt::{Inactive, Watchdog};"),
            "{body}"
        );
        assert!(
            body.contains("pub fn init(wdt: WDT) -> Result<Handle, WDT>"),
            "{body}"
        );
        assert!(body.contains("set_lfosc_ticks(TIMEOUT_TICKS)"), "{body}");
        // Paused at a breakpoint - nrf-hal's own default, said out loud.
        assert!(body.contains("run_during_debug_halt(false)"), "{body}");
        // The reflash note is THIS HAL's: nrf-hal refuses any running WDT. The
        // embassy wording ("adopts") would be false here.
        assert!(body.contains("nrf-hal refuses ANY watchdog"), "{body}");
        assert!(!body.contains("ADOPTS"), "{body}");
    }

    /// embassy-nrf starts the WDT in the call that configures it, so the
    /// generated block must not make that call: a fresh project's loop sleeps
    /// a minute, and a 1 s watchdog started for it would reset it forever.
    #[test]
    fn async_hands_over_the_peripheral_and_never_starts_it() {
        let mcu = microbit(Runtime::Async, Some(458));
        let main = mcu.fresh_main_rs();
        assert!(main.contains("let watchdog = p.WDT;"), "{main}");
        assert!(
            !main.contains("watchdog::start(watchdog);")
                && !main.contains("= pins::configs::watchdog"),
            "started from the generated block:\n{main}"
        );
        let body = file(&mcu);
        // 458 us is 15.008 ticks: rounded UP, never sooner than asked.
        assert!(body.contains("const TIMEOUT_TICKS: u32 = 16;"), "{body}");
        // `Config` is #[non_exhaustive]: a struct literal would not compile.
        assert!(
            body.contains("let mut config = Config::default();"),
            "{body}"
        );
        // embassy-nrf's default keeps it running under a debugger; ours pauses.
        assert!(body.contains("HaltConfig::Pause"), "{body}");
        assert!(body.contains("Watchdog::try_new(wdt, config)"), "{body}");
        // embassy-nrf adopts a watchdog left running with the SAME config, so
        // the note must not claim every leftover one is refused.
        assert!(
            body.contains("ADOPTS") && !body.contains("refuses ANY"),
            "{body}"
        );
        // The example sleeps half the PERIOD, not a fixed time a short period
        // would be outlasted by - 458 us here, well under any fixed 100 ms.
        assert!(body.contains("pub const TIMEOUT_US: u64 = 458;"), "{body}");
        assert!(
            body.contains("Timer::after_micros(pins::configs::watchdog::TIMEOUT_US / 2)"),
            "{body}"
        );
    }
}

#[cfg(test)]
mod shared_block {
    use super::blocking_codegen::microbit;
    use super::shared_block_partner;
    use crate::panels::mcu_module::mcu::{Mcu, Runtime};
    use crate::panels::mcu_module::modules::{ModuleKind, VirtualModule};
    use crate::panels::mcu_module::pins::PinFunction;

    fn module(kind: ModuleKind, n: u8) -> VirtualModule {
        VirtualModule {
            id: format!("{kind:?}_{n}"),
            kind,
            name: String::new(),
            pos: (0.0, 0.0),
            config: kind.default_config(n),
            connections: Vec::new(),
        }
    }

    /// What the panel says about SPIM`n` and TWIM`n` on this wiring, asked
    /// from both sides.
    fn partners(mcu: &Mcu, n: u8, is_async: bool) -> (Option<String>, Option<String>) {
        (
            shared_block_partner(mcu, &module(ModuleKind::GenericInterfaceSpi, n), is_async),
            shared_block_partner(mcu, &module(ModuleKind::GenericInterfaceI2c, n), is_async),
        )
    }

    fn on(mut mcu: Mcu, is_async: bool) -> Mcu {
        mcu.runtime = if is_async {
            Runtime::Async
        } else {
            Runtime::Blocking
        };
        mcu
    }

    const SCK: PinFunction = PinFunction::SpiSck(0);
    const MOSI: PinFunction = PinFunction::SpiMosi(0);
    const SCL: PinFunction = PinFunction::I2cScl(0);
    const SDA: PinFunction = PinFunction::I2cSda(0);

    /// The panel names a pair exactly when `main.rs` loses a bus to it: the
    /// clash comment on Async, both inits on Blocking. Run over the wirings
    /// where the two runtimes' half-wired rules differ, so the sentence can
    /// never point at a bus the generator actually built.
    #[test]
    fn a_pair_is_named_exactly_when_main_rs_loses_a_bus() {
        let wirings: [&[(&str, PinFunction)]; 5] = [
            // Both whole.
            &[
                ("P0.17", SCK),
                ("P0.13", MOSI),
                ("P0.08", SCL),
                ("P0.16", SDA),
            ],
            // A clock-only SPIM: nrf-hal builds it, embassy-nrf does not.
            &[("P0.17", SCK), ("P0.08", SCL), ("P0.16", SDA)],
            // No SCK: no SPIM on either runtime.
            &[("P0.13", MOSI), ("P0.08", SCL), ("P0.16", SDA)],
            // Half a TWIM: no TWIM on either runtime.
            &[("P0.17", SCK), ("P0.13", MOSI), ("P0.08", SCL)],
            // Only one of the two buses at all.
            &[("P0.17", SCK), ("P0.13", MOSI)],
        ];
        let expect = [
            (true, true),
            (false, true),
            (false, false),
            (false, false),
            (false, false),
        ];
        for (wire, (want_async, want_blocking)) in wirings.iter().zip(expect) {
            for (is_async, want) in [(true, want_async), (false, want_blocking)] {
                let mcu = on(microbit(wire), is_async);
                let main = mcu.fresh_main_rs();
                let lost = if is_async {
                    main.contains("// TWIM0 is not built: it is the same block as SPIM0")
                } else {
                    main.contains("pins::configs::spim0::init(")
                        && main.contains("pins::configs::twim0::init(")
                };
                assert_eq!(lost, want, "generator, async={is_async}, {wire:?}:\n{main}");
                let named = if want {
                    (Some("TWIM0".to_owned()), Some("SPIM0".to_owned()))
                } else {
                    (None, None)
                };
                assert_eq!(
                    partners(&mcu, 0, is_async),
                    named,
                    "async={is_async}, {wire:?}"
                );
            }
        }
    }

    /// TWISPI1 is the same rule on the other shared id.
    #[test]
    fn the_second_shared_block_pairs_too() {
        let mcu = microbit(&[
            ("P0.17", PinFunction::SpiSck(1)),
            ("P0.13", PinFunction::SpiMosi(1)),
            ("P0.08", PinFunction::I2cScl(1)),
            ("P0.16", PinFunction::I2cSda(1)),
        ]);
        for is_async in [true, false] {
            assert_eq!(
                partners(&mcu, 1, is_async),
                (Some("TWIM1".to_owned()), Some("SPIM1".to_owned()))
            );
        }
    }

    /// Different ids share nothing, SPIM2 has no TWIM twin, a UART is its own
    /// block, and no chip but the nRF gets the note.
    #[test]
    fn different_ids_other_kinds_and_other_chips_are_left_alone() {
        let crossed = microbit(&[
            ("P0.17", SCK),
            ("P0.13", MOSI),
            ("P0.08", PinFunction::I2cScl(1)),
            ("P0.16", PinFunction::I2cSda(1)),
        ]);
        assert_eq!(partners(&crossed, 0, true), (None, None));
        assert_eq!(partners(&crossed, 1, true), (None, None));

        let spim2 = microbit(&[
            ("P0.17", PinFunction::SpiSck(2)),
            ("P0.13", PinFunction::SpiMosi(2)),
            ("P0.08", PinFunction::I2cScl(2)),
            ("P0.16", PinFunction::I2cSda(2)),
        ]);
        assert_eq!(partners(&spim2, 2, true), (None, None));

        let full = microbit(&[
            ("P0.17", SCK),
            ("P0.13", MOSI),
            ("P0.08", SCL),
            ("P0.16", SDA),
        ]);
        let uart = module(ModuleKind::GenericInterfaceUsart, 0);
        assert_eq!(shared_block_partner(&full, &uart, true), None);
        for family in ["stm32f1", "stm32g0", "esp32c3", "rp2040"] {
            let mut other = microbit(&[
                ("P0.17", SCK),
                ("P0.13", MOSI),
                ("P0.08", SCL),
                ("P0.16", SDA),
            ]);
            other.family = family.to_owned();
            assert_eq!(partners(&other, 0, true), (None, None), "{family}");
        }
    }
}
