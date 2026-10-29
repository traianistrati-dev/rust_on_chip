//! What a link between two modules means on the wire: which pad meets which,
//! in which direction, and what does not match.
//!
//! A link names two modules; everything here is derived from the two chips as
//! they are NOW, every frame, and never stored. A pin re-assigned on the Pins
//! tab moves the wire; a module that is gone leaves the link BROKEN - drawn
//! as such and listed, never deleted behind the user's back.
//!
//! The rules, per bus:
//! - **UART**: TX to the other side's RX, both ways, as far as each side's
//!   direction uses them; RTS to CTS. A module with RX/TX swapped in hardware
//!   is read swapped. Two single-wire (half-duplex) ends share one DATA line,
//!   and such a bus may join many ends. Baud rate and frame must match; a
//!   two-wire UART joins exactly two ends. RS-485 (DE) runs through
//!   transceivers, so its RTS pad is not wired to the other chip.
//! - **SPI**: the master's SCK / MOSI / NSS to the slave, the slave's MISO
//!   back. Two masters or two slaves is a warning, and so is a mode mismatch.
//! - **I2C**: SCL to SCL, SDA to SDA - shared lines, no direction. The IDE
//!   generates controllers only, so two of them need a target written by hand.
//! - **CAN**: through a transceiver on each side, so no pin wires - unless
//!   both ends are set to work without one (ESP), when TX meets RX directly.
//! - **GPIO** (custom modules): pins paired in order; an output drives an
//!   input. Two push-pull outputs fight, two open-drain ones make a wired-AND,
//!   two inputs float.
//! - **I2S / SAI / USB**: the same signal on both sides, with the roles
//!   checked where the module has them.

use super::model::{Link, LinkEnd};
use super::parts::{CHIP_IO_MV, volts};
use super::snapshot::{ChipView, ModuleItem, SignalPin};
use crate::panels::mcu_module::modules::{
    I2sDirection, I2sMode, ModuleConfig, ModuleKind, ModuleSignal, SpiRole, UsartDirection,
    UsartFlow, UsartModuleConfig,
};
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

/// The kind of connection two modules make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bus {
    Uart,
    Spi,
    I2c,
    Can,
    Gpio,
    /// Same signal to same signal (I2S, SAI, USB).
    Same(ModuleKind),
}

/// The bus a module kind speaks, or `None` for one that cannot be linked to
/// another chip (a timer, a DAC, a camera input…).
pub fn bus_of(kind: ModuleKind) -> Option<Bus> {
    use ModuleKind::*;
    Some(match kind {
        GenericInterfaceUsart | GenericInterfaceLpuart => Bus::Uart,
        GenericInterfaceSpi => Bus::Spi,
        GenericInterfaceI2c => Bus::I2c,
        GenericInterfaceCan => Bus::Can,
        Custom => Bus::Gpio,
        GenericInterfaceI2s | GenericInterfaceSai | GenericInterfaceUsb => Bus::Same(kind),
        _ => return None,
    })
}

/// Whether modules of these two kinds can be linked.
pub fn can_link(a: ModuleKind, b: ModuleKind) -> bool {
    matches!((bus_of(a), bus_of(b)), (Some(x), Some(y)) if x == y)
}

/// The end a link records for a module: its chip, its identity, and - for a
/// custom module, whose number is handed out again after a remove - its name.
pub fn end_for(chip: &str, m: &ModuleItem) -> LinkEnd {
    LinkEnd {
        chip: chip.to_owned(),
        kind: m.kind,
        instance: m.instance,
        name: if m.kind.is_custom() {
            m.name.clone()
        } else {
            String::new()
        },
    }
}

/// Which way a signal travels on a wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    AtoB,
    BtoA,
    /// A shared line (I2C), or a direction nobody can tell.
    Both,
}

/// One end of a wire: a pad and what it carries there.
#[derive(Clone, Debug, PartialEq)]
pub struct PinEnd {
    pub pin: usize,
    pub pad: String,
    /// `TX`, `SCK`, `OUT`…
    pub role: String,
}

impl PinEnd {
    /// `TX PA9`, or just the role when the pad has no name.
    pub fn label(&self) -> String {
        if self.pad.is_empty() {
            self.role.clone()
        } else {
            format!("{} {}", self.role, self.pad)
        }
    }
}

/// One pin-to-pin wire of a link.
#[derive(Clone, Debug, PartialEq)]
pub struct Wire {
    pub a: PinEnd,
    pub b: PinEnd,
    pub dir: Dir,
}

/// A link as it stands now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resolved {
    /// (chip index, module index) of each end, when both are there.
    pub a: Option<(usize, usize)>,
    pub b: Option<(usize, usize)>,
    pub wires: Vec<Wire>,
    /// Things that do not match or cannot work.
    pub warnings: Vec<String>,
    /// Facts worth knowing that are not wrong (CAN needs transceivers).
    pub notes: Vec<String>,
    /// Why the link cannot be drawn at all.
    pub broken: Option<String>,
}

/// The name of a link end when its module may be gone: `USART1`, or for a
/// custom module the name it had.
pub fn end_name(end: &LinkEnd) -> String {
    use ModuleKind::*;
    if end.kind == Custom && !end.name.is_empty() {
        return end.name.clone();
    }
    let prefix = match end.kind {
        GenericInterfaceUsart => "USART",
        GenericInterfaceLpuart => "LPUART",
        GenericInterfaceSpi => "SPI",
        GenericInterfaceI2c => "I2C",
        GenericInterfaceCan => "CAN",
        GenericInterfaceI2s => "I2S",
        GenericInterfaceSai => "SAI",
        GenericInterfaceUsb => "USB",
        Custom => "custom module ",
        _ => "module ",
    };
    format!("{prefix}{}", end.instance)
}

fn find(end: &LinkEnd, views: &[ChipView]) -> Result<(usize, usize), String> {
    let Some(c) = views
        .iter()
        .position(|v| v.dir.eq_ignore_ascii_case(&end.chip))
    else {
        return Err(format!("{} is not in the system", end.chip));
    };
    match views[c].module(end.kind, end.instance) {
        // The same number on a custom module that is not the one linked:
        // numbers are reused, names are what the user sees.
        Some(m)
            if end.kind.is_custom()
                && !end.name.is_empty()
                && views[c].modules[m].name != end.name =>
        {
            Err(format!(
                "{} is gone from {} - its number now belongs to {}",
                end.name, end.chip, views[c].modules[m].name
            ))
        }
        Some(m) => Ok((c, m)),
        None => match &views[c].problem {
            Some(p) if views[c].modules.is_empty() => Err(format!("{}: {p}", end.chip)),
            _ => Err(format!("{} is no longer on {}", end_name(end), end.chip)),
        },
    }
}

/// Work out one link against the chips as they are now. `all` is every link
/// of the system (a two-wire UART in two links is a warning).
pub fn resolve(link: &Link, views: &[ChipView], all: &[Link]) -> Resolved {
    let (a, b) = match (find(&link.a, views), find(&link.b, views)) {
        (Ok(a), Ok(b)) => (a, b),
        (a, b) => {
            let why = [a.as_ref().err(), b.as_ref().err()]
                .into_iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>();
            return Resolved {
                a: a.ok(),
                b: b.ok(),
                broken: Some(why.join("; ")),
                ..Default::default()
            };
        }
    };
    let (ma, mb) = (&views[a.0].modules[a.1], &views[b.0].modules[b.1]);
    let mut r = Resolved {
        a: Some(a),
        b: Some(b),
        ..Default::default()
    };
    let (na, nb) = (
        format!("{} {}", link.a.chip, ma.name),
        format!("{} {}", link.b.chip, mb.name),
    );
    match (bus_of(ma.kind), bus_of(mb.kind)) {
        (Some(x), Some(y)) if x == y => match x {
            Bus::Uart => uart(&mut r, ma, mb, &na, &nb, link, all),
            Bus::Spi => spi(&mut r, ma, mb, &na, &nb),
            Bus::I2c => i2c(
                &mut r,
                ma,
                mb,
                &na,
                &nb,
                [
                    views[a.0].external_mv.is_some(),
                    views[b.0].external_mv.is_some(),
                ],
            ),
            Bus::Can => can(&mut r, ma, mb, &na, &nb),
            Bus::Gpio => gpio(&mut r, ma, mb, &na, &nb),
            Bus::Same(kind) => same(&mut r, kind, ma, mb, &na, &nb),
        },
        _ => r.broken = Some(format!("{} cannot be linked to {}", ma.name, mb.name)),
    }
    // I/O levels, where an external part says what its are. A chip project
    // is taken as 3.3 V: every chip this IDE knows runs its pins there.
    // Only where pins meet: a CAN link through transceivers shares no pin,
    // and each side's level stops at its own transceiver.
    let (va, vb) = (views[a.0].external_mv, views[b.0].external_mv);
    if (va.is_some() || vb.is_some()) && !r.wires.is_empty() {
        let (va, vb) = (va.unwrap_or(CHIP_IO_MV), vb.unwrap_or(CHIP_IO_MV));
        if va != vb {
            r.warnings.push(format!(
                "I/O levels differ: {na} at {}, {nb} at {} - it needs a level shifter",
                volts(va),
                volts(vb)
            ));
        }
    }
    r
}

fn end_of(s: &SignalPin, role: &str) -> PinEnd {
    PinEnd {
        pin: s.pin,
        pad: s.pad.clone(),
        role: role.to_owned(),
    }
}

fn signal<'a>(m: &'a ModuleItem, wanted: &[ModuleSignal]) -> Option<&'a SignalPin> {
    m.signals.iter().find(|s| wanted.contains(&s.signal))
}

fn uart_cfg(m: &ModuleItem) -> Option<&UsartModuleConfig> {
    match &m.config {
        ModuleConfig::Usart(c) | ModuleConfig::Lpuart(c) => Some(c),
        _ => None,
    }
}

fn uart_direction(m: &ModuleItem) -> UsartDirection {
    uart_cfg(m).map_or(UsartDirection::TxRx, |c| c.direction)
}

/// TX, RX, CTS, RTS of a two-wire UART as the wire sees them: only the lines
/// its direction uses, RX/TX swapped when the hardware swaps them, and no RTS
/// when that pad drives an RS-485 transceiver (it stays on this board).
fn uart_pins(m: &ModuleItem) -> [Option<&SignalPin>; 4] {
    use ModuleSignal::*;
    let d = uart_direction(m);
    let tx = signal(m, &[Tx, LpTx]).filter(|_| d.needs_tx());
    let rx = signal(m, &[Rx, LpRx]).filter(|_| d.needs_rx());
    let swapped = uart_cfg(m).is_some_and(|c| c.swap_rx_tx);
    let (tx, rx) = if swapped { (rx, tx) } else { (tx, rx) };
    let de = uart_cfg(m).is_some_and(|c| c.flow == UsartFlow::De);
    let rts = signal(m, &[Rts, LpRts]).filter(|_| !de);
    [tx, rx, signal(m, &[Cts, LpCts]), rts]
}

/// The one data pad of a single-wire UART: its TX pad or its RX pad.
fn half_duplex_line(m: &ModuleItem) -> Option<&SignalPin> {
    use ModuleSignal::*;
    match uart_direction(m) {
        UsartDirection::HalfDuplexOnTx => signal(m, &[Tx, LpTx]),
        UsartDirection::HalfDuplexOnRx => signal(m, &[Rx, LpRx]),
        _ => None,
    }
}

fn uart(
    r: &mut Resolved,
    ma: &ModuleItem,
    mb: &ModuleItem,
    na: &str,
    nb: &str,
    link: &Link,
    all: &[Link],
) {
    // A UART its chip's generated code does not build has no line to draw.
    let unbuilt: Vec<(&str, &str)> = [(ma, na), (mb, nb)]
        .into_iter()
        .filter_map(|(m, n)| m.not_built.as_deref().map(|why| (n, why)))
        .collect();
    if !unbuilt.is_empty() {
        for (n, why) in unbuilt {
            r.warnings.push(format!("{n} is not built: {why}"));
        }
        return;
    }
    let (ha, hb) = (
        uart_direction(ma).is_half_duplex(),
        uart_direction(mb).is_half_duplex(),
    );
    if ha && hb {
        match (half_duplex_line(ma), half_duplex_line(mb)) {
            (Some(x), Some(y)) => r.wires.push(Wire {
                a: end_of(x, "DATA"),
                b: end_of(y, "DATA"),
                dir: Dir::Both,
            }),
            _ => r
                .warnings
                .push("A single-wire end has no data pad".to_owned()),
        }
    } else if ha || hb {
        let (single, double) = if ha { (na, nb) } else { (nb, na) };
        r.warnings.push(format!(
            "{single} is single-wire (half duplex), {double} is two-wire - they cannot share a line"
        ));
    } else {
        let [a_tx, a_rx, a_cts, a_rts] = uart_pins(ma);
        let [b_tx, b_rx, b_cts, b_rts] = uart_pins(mb);
        let mut pair =
            |from: Option<&SignalPin>, fr: &str, to: Option<&SignalPin>, tr: &str, dir| {
                if let (Some(f), Some(t)) = (from, to) {
                    let (a, b) = match dir {
                        Dir::BtoA => (end_of(t, tr), end_of(f, fr)),
                        _ => (end_of(f, fr), end_of(t, tr)),
                    };
                    r.wires.push(Wire { a, b, dir });
                }
            };
        pair(a_tx, "TX", b_rx, "RX", Dir::AtoB);
        pair(b_tx, "TX", a_rx, "RX", Dir::BtoA);
        pair(a_rts, "RTS", b_cts, "CTS", Dir::AtoB);
        pair(b_rts, "RTS", a_cts, "CTS", Dir::BtoA);
        if a_tx.is_some() && b_rx.is_none() {
            r.warnings
                .push(format!("{na} TX has no RX to reach on {nb}"));
        }
        if b_tx.is_some() && a_rx.is_none() {
            r.warnings
                .push(format!("{nb} TX has no RX to reach on {na}"));
        }
        // A CTS waits for the other side's RTS; with none it never clears.
        if a_cts.is_some() && b_rts.is_none() {
            r.warnings
                .push(format!("{na} CTS has no RTS to clear it on {nb}"));
        }
        if b_cts.is_some() && a_rts.is_none() {
            r.warnings
                .push(format!("{nb} CTS has no RTS to clear it on {na}"));
        }
    }
    for (m, name) in [(ma, na), (mb, nb)] {
        if uart_cfg(m).is_some_and(|c| c.flow == UsartFlow::De) {
            r.notes.push(format!(
                "{name} drives an RS-485 transceiver: the line runs over A/B, its DE pad stays on its own board"
            ));
        }
    }
    if let (Some(x), Some(y)) = (uart_cfg(ma), uart_cfg(mb)) {
        if x.baud_rate != y.baud_rate {
            r.warnings
                .push(format!("Baud rate {} vs {}", x.baud_rate, y.baud_rate));
        }
        let frame = |c: &UsartModuleConfig| {
            (
                c.data_bits,
                format!("{:?}", c.parity),
                format!("{:?}", c.stop_bits),
            )
        };
        if frame(x) != frame(y) {
            let show = |c: &UsartModuleConfig| {
                let (d, p, s) = frame(c);
                format!("{d} data bits, parity {p}, stop {s}")
            };
            r.warnings.push(format!("Frame {} vs {}", show(x), show(y)));
        }
    }
    // Point-to-point - unless it is a single-wire bus (servos, DMX), which
    // joins many ends by design, or RS-485, which is multi-drop too.
    for (end, m) in [(&link.a, ma), (&link.b, mb)] {
        let bus = uart_direction(m).is_half_duplex()
            || uart_cfg(m).is_some_and(|c| c.flow == UsartFlow::De);
        let n = all
            .iter()
            .filter(|l| l.a.same_as(end) || l.b.same_as(end))
            .count();
        if n > 1 && !bus {
            r.warnings.push(format!(
                "{} on {} is in {n} links - a UART joins exactly two ends",
                end_name(end),
                end.chip
            ));
        }
    }
}

fn spi(r: &mut Resolved, ma: &ModuleItem, mb: &ModuleItem, na: &str, nb: &str) {
    use ModuleSignal::*;
    let role = |m: &ModuleItem| match &m.config {
        ModuleConfig::Spi(c) => c.role,
        _ => SpiRole::Master,
    };
    let (ra, rb) = (role(ma), role(mb));
    // The master is where the clock comes from. Two of a kind: the first end
    // is drawn as the master, and the warning says why that is a guess.
    let a_is_master = match (ra, rb) {
        (SpiRole::Master, SpiRole::Slave) => true,
        (SpiRole::Slave, SpiRole::Master) => false,
        (SpiRole::Master, SpiRole::Master) => {
            r.warnings.push(format!(
                "{na} and {nb} are both SPI masters - one has to be the slave"
            ));
            true
        }
        (SpiRole::Slave, SpiRole::Slave) => {
            r.warnings.push(format!(
                "{na} and {nb} are both SPI slaves - nothing drives the clock"
            ));
            true
        }
    };
    let (m, s, to_slave, to_master) = if a_is_master {
        (ma, mb, Dir::AtoB, Dir::BtoA)
    } else {
        (mb, ma, Dir::BtoA, Dir::AtoB)
    };
    let mut pair = |sig: ModuleSignal, role: &str, dir: Dir| {
        if let (Some(x), Some(y)) = (signal(ma, &[sig]), signal(mb, &[sig])) {
            r.wires.push(Wire {
                a: end_of(x, role),
                b: end_of(y, role),
                dir,
            });
        }
    };
    pair(Sck, "SCK", to_slave);
    pair(Mosi, "MOSI", to_slave);
    pair(Miso, "MISO", to_master);
    pair(Nss, "NSS", to_slave);
    if signal(s, &[Nss]).is_some() && signal(m, &[Nss]).is_none() {
        r.warnings.push(
            "The slave's NSS has no chip-select from the master - drive it from a GPIO or tie it"
                .to_owned(),
        );
    }
    for (sig, name) in [(Sck, "SCK"), (Mosi, "MOSI"), (Miso, "MISO")] {
        if signal(ma, &[sig]).is_some() != signal(mb, &[sig]).is_some() {
            r.warnings.push(format!("{name} is wired on one side only"));
        }
    }
    if let (ModuleConfig::Spi(x), ModuleConfig::Spi(y)) = (&ma.config, &mb.config)
        && x.mode != y.mode
    {
        r.warnings
            .push(format!("SPI mode {} vs {}", x.mode, y.mode));
    }
}

/// `external` says which ends are external parts - an I2C target each; a
/// chip project's I2C module is always generated as a controller.
fn i2c(
    r: &mut Resolved,
    ma: &ModuleItem,
    mb: &ModuleItem,
    na: &str,
    nb: &str,
    external: [bool; 2],
) {
    use ModuleSignal::*;
    for (sig, name) in [(Scl, "SCL"), (Sda, "SDA")] {
        match (signal(ma, &[sig]), signal(mb, &[sig])) {
            (Some(x), Some(y)) => r.wires.push(Wire {
                a: end_of(x, name),
                b: end_of(y, name),
                dir: Dir::Both,
            }),
            _ => r.warnings.push(format!("{name} is wired on one side only")),
        }
    }
    // A target follows whatever clock the controller runs.
    if let (ModuleConfig::I2c(x), ModuleConfig::I2c(y)) = (&ma.config, &mb.config)
        && x.clock_hz != y.clock_hz
        && external == [false, false]
    {
        r.warnings.push(format!(
            "I2C clock {} vs {}",
            crate::panels::mcu_module::modules::model::hz_label(x.clock_hz),
            crate::panels::mcu_module::modules::model::hz_label(y.clock_hz)
        ));
    }
    // Every I2C module the IDE generates is a controller; a part is a target.
    if external == [true, true] {
        r.warnings.push(format!(
            "{na} and {nb} are both I2C targets - nothing drives SCL"
        ));
    }
    if external == [false, false] {
        r.warnings.push(format!(
            "{na} and {nb} are both generated as I2C controllers - one side has to answer as a target (hand-written)"
        ));
    }
    r.notes
        .push("I2C needs one pull-up on SCL and one on SDA".to_owned());
}

fn can_cfg(m: &ModuleItem) -> Option<&crate::panels::mcu_module::modules::CanModuleConfig> {
    match &m.config {
        ModuleConfig::Can(c) => Some(c),
        _ => None,
    }
}

fn can(r: &mut Resolved, ma: &ModuleItem, mb: &ModuleItem, na: &str, nb: &str) {
    use ModuleSignal::*;
    let (ta, tb) = (
        can_cfg(ma).is_none_or(|c| c.transceiver),
        can_cfg(mb).is_none_or(|c| c.transceiver),
    );
    match (ta, tb) {
        (true, true) => r.notes.push(
            "CAN runs through a transceiver on each side (CANH/CANL), not pin to pin".to_owned(),
        ),
        (false, false) => {
            // No transceivers (an ESP bench setup): the pads meet directly.
            for (from, to, dir) in [(ma, mb, Dir::AtoB), (mb, ma, Dir::BtoA)] {
                if let (Some(f), Some(t)) = (signal(from, &[CanTx]), signal(to, &[CanRx])) {
                    let (a, b) = match dir {
                        Dir::BtoA => (end_of(t, "RX"), end_of(f, "TX")),
                        _ => (end_of(f, "TX"), end_of(t, "RX")),
                    };
                    r.wires.push(Wire { a, b, dir });
                } else {
                    r.warnings
                        .push("A CAN end has no TX or RX pad to wire".to_owned());
                }
            }
            r.notes.push(
                "No transceivers: the pads are wired directly - TX open-drain, one pull-up on the line"
                    .to_owned(),
            );
        }
        _ => {
            let (with, without) = if ta { (na, nb) } else { (nb, na) };
            r.warnings.push(format!(
                "{with} expects a transceiver, {without} is wired pad to pad - they cannot talk"
            ));
        }
    }
    if let (Some(x), Some(y)) = (can_cfg(ma), can_cfg(mb))
        && x.bitrate != y.bitrate
    {
        r.warnings
            .push(format!("Bit rate {} vs {}", x.bitrate, y.bitrate));
    }
}

/// `Some(true)` for a pad that drives its line, `Some(false)` for one that
/// reads it, `None` when it says neither.
fn drives(f: &PinFunction) -> Option<bool> {
    match f {
        PinFunction::GpioOutput | PinFunction::TimerPwm { .. } | PinFunction::TimerPwmN { .. } => {
            Some(true)
        }
        PinFunction::GpioInput => Some(false),
        _ => None,
    }
}

fn gpio(r: &mut Resolved, ma: &ModuleItem, mb: &ModuleItem, na: &str, nb: &str) {
    let role = |d: Option<bool>| match d {
        Some(true) => "OUT",
        Some(false) => "IN",
        None => "IO",
    };
    for (x, y) in ma.signals.iter().zip(&mb.signals) {
        let (dx, dy) = (drives(&x.function), drives(&y.function));
        let dir = match (dx, dy) {
            (Some(true), Some(false)) => Dir::AtoB,
            (Some(false), Some(true)) => Dir::BtoA,
            (Some(true), Some(true)) if x.open_drain && y.open_drain => {
                r.notes.push(format!(
                    "{} and {} are open-drain: a wired-AND line, it needs one pull-up",
                    x.pad, y.pad
                ));
                Dir::Both
            }
            (Some(true), Some(true)) => {
                r.warnings.push(format!(
                    "{} and {} are both outputs - they would fight",
                    x.pad, y.pad
                ));
                Dir::Both
            }
            (Some(false), Some(false)) => {
                r.warnings.push(format!(
                    "{} and {} are both inputs - nothing drives the line",
                    x.pad, y.pad
                ));
                Dir::Both
            }
            _ => {
                for (p, d) in [(x, dx), (y, dy)] {
                    if d.is_none() {
                        r.warnings
                            .push(format!("{} is neither an input nor an output", p.pad));
                    }
                }
                Dir::Both
            }
        };
        r.wires.push(Wire {
            a: end_of(x, role(dx)),
            b: end_of(y, role(dy)),
            dir,
        });
    }
    if ma.signals.len() != mb.signals.len() {
        r.warnings.push(format!(
            "{na} has {} pins, {nb} has {} - the extra ones are not wired",
            ma.signals.len(),
            mb.signals.len()
        ));
    }
}

fn same(r: &mut Resolved, kind: ModuleKind, ma: &ModuleItem, mb: &ModuleItem, na: &str, nb: &str) {
    for x in &ma.signals {
        if let Some(y) = mb.signals.iter().find(|y| y.signal == x.signal) {
            r.wires.push(Wire {
                a: end_of(x, x.signal.label()),
                b: end_of(y, y.signal.label()),
                dir: Dir::Both,
            });
        }
    }
    match (kind, &ma.config, &mb.config) {
        (_, ModuleConfig::I2s(x), ModuleConfig::I2s(y)) => {
            if x.mode == y.mode {
                let what = match x.mode {
                    I2sMode::Master => "masters - both drive the clocks",
                    I2sMode::Slave => "slaves - nothing drives the clocks",
                };
                r.warnings
                    .push(format!("{na} and {nb} are both I2S {what}"));
            }
            if x.direction == y.direction {
                let what = match x.direction {
                    I2sDirection::Transmit => "transmit - nothing receives",
                    I2sDirection::Receive => "receive - nothing sends",
                };
                r.warnings.push(format!("{na} and {nb} both {what}"));
            }
            if x.sample_rate_hz != y.sample_rate_hz {
                r.warnings.push(format!(
                    "Sample rate {} Hz vs {} Hz",
                    x.sample_rate_hz, y.sample_rate_hz
                ));
            }
        }
        (ModuleKind::GenericInterfaceUsb, _, _) => r.warnings.push(
            "USB joins a host and a device - every USB module the IDE generates is a device"
                .to_owned(),
        ),
        _ => {}
    }
}

/// How a module's pad reads at the frame edge: `TX PA9`, `OUT PB0`.
fn pad_label(m: &ModuleItem, s: &SignalPin) -> String {
    let role = if m.kind.is_custom() {
        match drives(&s.function) {
            Some(true) => "OUT",
            Some(false) => "IN",
            None => "IO",
        }
    } else {
        s.signal.label()
    };
    if s.pad.is_empty() {
        role.to_owned()
    } else {
        format!("{role} {}", s.pad)
    }
}

/// How each frame arranges its modules, from the links: a linked module faces
/// the chip it links to, ranked by where on that chip its partner sits, so
/// links run without crossing; a module with no link joins the side most of
/// the linked ones are on (the right, when none are).
///
/// In the Detailed view EVERY module lists its pads, linked or not - that is
/// what makes the view differ from Abstract on a chip that has no link yet.
/// A pad a link uses takes the link's own name for it (`DATA` on a
/// single-wire UART, `IN`/`OUT` on a GPIO line).
///
/// `frames` is each chip's top-left and size on the canvas (the size may be a
/// frame's plain one - only the centres matter here).
pub fn arrange(
    views: &[ChipView],
    frames: &[(eframe::egui::Pos2, eframe::egui::Vec2)],
    resolved: &[Resolved],
    detailed: bool,
) -> Vec<super::layout::Arrange> {
    use super::layout::{Arrange, PILL_H, ROW_GAP, Side};
    let centre = |c: usize| frames.get(c).map_or(0.0, |(p, s)| p.x + s.x / 2.0);
    let top = |c: usize| frames.get(c).map_or(0.0, |(p, _)| p.y);
    // Per chip, per module: the (side, rank) of each partner.
    let mut seen: Vec<Vec<Vec<(Side, f32)>>> = views
        .iter()
        .map(|v| vec![Vec::new(); v.modules.len()])
        .collect();
    let mut out: Vec<Arrange> = views.iter().map(Arrange::plain).collect();
    if detailed {
        for (c, v) in views.iter().enumerate() {
            for (m, item) in v.modules.iter().enumerate() {
                out[c].pins[m] = item
                    .signals
                    .iter()
                    .map(|s| (s.pin, pad_label(item, s)))
                    .collect();
            }
        }
    }
    for r in resolved.iter().filter(|r| r.broken.is_none()) {
        let (Some((ca, ma)), Some((cb, mb))) = (r.a, r.b) else {
            continue;
        };
        for ((c, m), (pc, pm)) in [((ca, ma), (cb, mb)), ((cb, mb), (ca, ma))] {
            let side = if centre(pc) < centre(c) {
                Side::Left
            } else {
                Side::Right
            };
            seen[c][m].push((side, top(pc) + pm as f32 * (PILL_H + ROW_GAP)));
        }
        if detailed {
            for w in &r.wires {
                for ((c, m), end) in [((ca, ma), &w.a), ((cb, mb), &w.b)] {
                    let pins = &mut out[c].pins[m];
                    match pins.iter_mut().find(|(n, _)| *n == end.pin) {
                        Some(row) => row.1 = end.label(),
                        None => pins.push((end.pin, end.label())),
                    }
                }
            }
        }
    }
    for (c, a) in out.iter_mut().enumerate() {
        let mut left = 0;
        let mut right = 0;
        for (m, partners) in seen[c].iter().enumerate() {
            if partners.is_empty() {
                continue;
            }
            let l = partners.iter().filter(|(s, _)| *s == Side::Left).count();
            let side = if l * 2 > partners.len() {
                Side::Left
            } else {
                Side::Right
            };
            match side {
                Side::Left => left += 1,
                Side::Right => right += 1,
            }
            a.side[m] = side;
            a.rank[m] = partners
                .iter()
                .map(|(_, k)| *k)
                .fold(f32::INFINITY, f32::min);
        }
        let rest = if left > right {
            Side::Left
        } else {
            Side::Right
        };
        for (m, partners) in seen[c].iter().enumerate() {
            if partners.is_empty() {
                a.side[m] = rest;
                // After every linked module, in list order.
                a.rank[m] = 1.0e6 + m as f32;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::modules::{
        CanModuleConfig, I2cModuleConfig, SpiModuleConfig, UsartModuleConfig,
        model::CustomModuleConfig,
    };

    fn sp(signal: ModuleSignal, pin: usize, pad: &str) -> SignalPin {
        SignalPin {
            signal,
            pin,
            pad: pad.to_owned(),
            function: PinFunction::Unset,
            open_drain: false,
        }
    }

    fn module(
        kind: ModuleKind,
        instance: u8,
        config: ModuleConfig,
        signals: Vec<SignalPin>,
    ) -> ModuleItem {
        ModuleItem {
            name: format!("{:?}{instance}", kind),
            kind,
            instance,
            pins: signals.iter().map(|s| s.pin).collect(),
            signals,
            config,
            not_built: None,
        }
    }

    fn chip(dir: &str, modules: Vec<ModuleItem>) -> ChipView {
        ChipView {
            dir: dir.to_owned(),
            chip: "X".into(),
            runtime: None,
            modules,
            devices: vec![],
            problem: None,
            external_mv: None,
        }
    }

    fn end(chip: &str, kind: ModuleKind, instance: u8) -> LinkEnd {
        LinkEnd {
            chip: chip.to_owned(),
            kind,
            instance,
            name: String::new(),
        }
    }

    fn usart(instance: u8, baud: u32, tx: (usize, &str), rx: (usize, &str)) -> ModuleItem {
        let mut c = UsartModuleConfig::new(instance);
        c.baud_rate = baud;
        module(
            ModuleKind::GenericInterfaceUsart,
            instance,
            ModuleConfig::Usart(c),
            vec![
                sp(ModuleSignal::Tx, tx.0, tx.1),
                sp(ModuleSignal::Rx, rx.0, rx.1),
            ],
        )
    }

    fn uart_link() -> Link {
        Link {
            a: end("stm32_main", ModuleKind::GenericInterfaceUsart, 1),
            b: end("esp32_radio", ModuleKind::GenericInterfaceUsart, 0),
        }
    }

    fn pads(r: &Resolved) -> Vec<(String, String, Dir)> {
        r.wires
            .iter()
            .map(|w| (w.a.label(), w.b.label(), w.dir))
            .collect()
    }

    /// TX meets RX both ways, and a baud rate that differs is said.
    #[test]
    fn a_uart_crosses_tx_and_rx() {
        let views = [
            chip(
                "stm32_main",
                vec![usart(1, 115_200, (30, "PA9"), (31, "PA10"))],
            ),
            chip(
                "esp32_radio",
                vec![usart(0, 9_600, (20, "GPIO21"), (21, "GPIO20"))],
            ),
        ];
        let r = resolve(&uart_link(), &views, &[uart_link()]);
        assert_eq!(r.broken, None);
        assert_eq!(
            pads(&r),
            [
                ("TX PA9".into(), "RX GPIO20".into(), Dir::AtoB),
                ("RX PA10".into(), "TX GPIO21".into(), Dir::BtoA),
            ]
        );
        assert_eq!(r.warnings, ["Baud rate 115200 vs 9600"]);
    }

    /// RX/TX swapped in hardware: the pad the RX signal names is the one that
    /// transmits, so it meets the other side's RX.
    #[test]
    fn a_swapped_uart_is_read_swapped() {
        let mut a = usart(1, 115_200, (30, "PA9"), (31, "PA10"));
        if let ModuleConfig::Usart(c) = &mut a.config {
            c.swap_rx_tx = true;
        }
        let views = [
            chip("stm32_main", vec![a]),
            chip(
                "esp32_radio",
                vec![usart(0, 115_200, (20, "GPIO21"), (21, "GPIO20"))],
            ),
        ];
        let r = resolve(&uart_link(), &views, &[uart_link()]);
        assert_eq!(
            pads(&r)[0],
            ("TX PA10".into(), "RX GPIO20".into(), Dir::AtoB)
        );
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    /// A UART in two links is point-to-point being broken.
    #[test]
    fn a_uart_in_two_links_is_warned_about() {
        let other = Link {
            a: end("stm32_main", ModuleKind::GenericInterfaceUsart, 1),
            b: end("pico", ModuleKind::GenericInterfaceUsart, 0),
        };
        let views = [
            chip(
                "stm32_main",
                vec![usart(1, 115_200, (30, "PA9"), (31, "PA10"))],
            ),
            chip(
                "esp32_radio",
                vec![usart(0, 115_200, (20, "GPIO21"), (21, "GPIO20"))],
            ),
        ];
        let r = resolve(&uart_link(), &views, &[uart_link(), other]);
        assert!(
            r.warnings.iter().any(|w| w.contains("in 2 links")),
            "{:?}",
            r.warnings
        );
    }

    fn spi_mod(instance: u8, role: SpiRole, mode: u8, nss: bool) -> ModuleItem {
        let mut c = SpiModuleConfig::new(instance);
        c.role = role;
        c.mode = mode;
        let mut s = vec![
            sp(ModuleSignal::Sck, 1, "SCK"),
            sp(ModuleSignal::Mosi, 2, "MOSI"),
            sp(ModuleSignal::Miso, 3, "MISO"),
        ];
        if nss {
            s.push(sp(ModuleSignal::Nss, 4, "NSS"));
        }
        module(
            ModuleKind::GenericInterfaceSpi,
            instance,
            ModuleConfig::Spi(c),
            s,
        )
    }

    fn spi_link() -> Link {
        Link {
            a: end("a", ModuleKind::GenericInterfaceSpi, 1),
            b: end("b", ModuleKind::GenericInterfaceSpi, 2),
        }
    }

    /// Whichever end is the master, the clock flows from it and MISO back.
    #[test]
    fn spi_flows_from_the_master() {
        for (ra, rb, to_slave) in [
            (SpiRole::Master, SpiRole::Slave, Dir::AtoB),
            (SpiRole::Slave, SpiRole::Master, Dir::BtoA),
        ] {
            let views = [
                chip("a", vec![spi_mod(1, ra, 0, true)]),
                chip("b", vec![spi_mod(2, rb, 0, true)]),
            ];
            let r = resolve(&spi_link(), &views, &[]);
            let dirs: Vec<(&str, Dir)> =
                r.wires.iter().map(|w| (w.a.role.as_str(), w.dir)).collect();
            let back = if to_slave == Dir::AtoB {
                Dir::BtoA
            } else {
                Dir::AtoB
            };
            assert_eq!(
                dirs,
                [
                    ("SCK", to_slave),
                    ("MOSI", to_slave),
                    ("MISO", back),
                    ("NSS", to_slave)
                ]
            );
            assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        }
    }

    #[test]
    fn two_spi_masters_a_mode_mismatch_and_an_orphan_nss_are_warned_about() {
        let views = [
            chip("a", vec![spi_mod(1, SpiRole::Master, 0, false)]),
            chip("b", vec![spi_mod(2, SpiRole::Master, 3, true)]),
        ];
        let w = resolve(&spi_link(), &views, &[]).warnings;
        assert!(w.iter().any(|x| x.contains("both SPI masters")), "{w:?}");
        assert!(w.iter().any(|x| x == "SPI mode 0 vs 3"), "{w:?}");
        let views = [
            chip("a", vec![spi_mod(1, SpiRole::Master, 0, false)]),
            chip("b", vec![spi_mod(2, SpiRole::Slave, 0, true)]),
        ];
        let w = resolve(&spi_link(), &views, &[]).warnings;
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("NSS has no chip-select"));
    }

    /// I2C lines are shared: no direction, a clock that differs is said, a
    /// missing line is said.
    #[test]
    fn i2c_shares_its_two_lines() {
        let i2c = |inst: u8, hz: u32, sda: bool| {
            let mut c = I2cModuleConfig::new(inst);
            c.clock_hz = hz;
            let mut s = vec![sp(ModuleSignal::Scl, 1, "SCL")];
            if sda {
                s.push(sp(ModuleSignal::Sda, 2, "SDA"));
            }
            module(
                ModuleKind::GenericInterfaceI2c,
                inst,
                ModuleConfig::I2c(c),
                s,
            )
        };
        let link = Link {
            a: end("a", ModuleKind::GenericInterfaceI2c, 1),
            b: end("b", ModuleKind::GenericInterfaceI2c, 0),
        };
        let views = [
            chip("a", vec![i2c(1, 100_000, true)]),
            chip("b", vec![i2c(0, 400_000, false)]),
        ];
        let r = resolve(&link, &views, &[]);
        assert_eq!(r.wires.len(), 1);
        assert_eq!(r.wires[0].dir, Dir::Both);
        assert!(
            r.warnings
                .contains(&"SDA is wired on one side only".to_owned())
        );
        assert!(
            r.warnings.iter().any(|w| w.starts_with("I2C clock")),
            "{:?}",
            r.warnings
        );
    }

    /// CAN is never pin to pin.
    #[test]
    fn can_draws_no_pin_wires() {
        let can_mod = |bitrate: u32| {
            let mut c = CanModuleConfig::new(1);
            c.bitrate = bitrate;
            module(
                ModuleKind::GenericInterfaceCan,
                1,
                ModuleConfig::Can(c),
                vec![
                    sp(ModuleSignal::CanTx, 1, "PB9"),
                    sp(ModuleSignal::CanRx, 2, "PB8"),
                ],
            )
        };
        let link = Link {
            a: end("a", ModuleKind::GenericInterfaceCan, 1),
            b: end("b", ModuleKind::GenericInterfaceCan, 1),
        };
        let views = [
            chip("a", vec![can_mod(500_000)]),
            chip("b", vec![can_mod(250_000)]),
        ];
        let r = resolve(&link, &views, &[]);
        assert!(r.wires.is_empty());
        assert_eq!(r.warnings, ["Bit rate 500000 vs 250000"]);
        assert!(!r.notes.is_empty());
    }

    fn custom(instance: u8, pins: &[(usize, &str, PinFunction)]) -> ModuleItem {
        let mut c = CustomModuleConfig::new(instance);
        c.pins = pins.iter().map(|p| p.0).collect();
        module(
            ModuleKind::Custom,
            instance,
            ModuleConfig::Custom(c),
            pins.iter()
                .map(|(n, pad, f)| SignalPin {
                    signal: ModuleSignal::CustomPin,
                    pin: *n,
                    pad: (*pad).to_owned(),
                    function: f.clone(),
                    open_drain: false,
                })
                .collect(),
        )
    }

    /// GPIO pins pair in order: an output drives an input; two outputs fight,
    /// two inputs float, an extra pin is not wired.
    #[test]
    fn gpio_pins_pair_in_order_and_outputs_drive_inputs() {
        use PinFunction::{GpioInput, GpioOutput};
        let link = Link {
            a: end("a", ModuleKind::Custom, 0),
            b: end("b", ModuleKind::Custom, 1),
        };
        let views = [
            chip(
                "a",
                vec![custom(
                    0,
                    &[
                        (1, "PB0", GpioInput),
                        (2, "PB1", GpioOutput),
                        (3, "PB2", GpioOutput),
                    ],
                )],
            ),
            chip(
                "b",
                vec![custom(
                    1,
                    &[(9, "GPIO4", GpioOutput), (8, "GPIO5", GpioOutput)],
                )],
            ),
        ];
        let r = resolve(&link, &views, &[]);
        let got: Vec<(String, String, Dir)> = pads(&r);
        assert_eq!(
            got,
            [
                ("IN PB0".into(), "OUT GPIO4".into(), Dir::BtoA),
                ("OUT PB1".into(), "OUT GPIO5".into(), Dir::Both),
            ]
        );
        assert!(
            r.warnings.iter().any(|w| w.contains("both outputs")),
            "{:?}",
            r.warnings
        );
        assert!(
            r.warnings.iter().any(|w| w.contains("3 pins")),
            "{:?}",
            r.warnings
        );
    }

    /// A link whose module or chip is gone is BROKEN - with the reason - and
    /// keeps the end that is still there.
    #[test]
    fn a_link_to_something_gone_is_broken_not_dropped() {
        let views = [chip(
            "stm32_main",
            vec![usart(1, 115_200, (30, "PA9"), (31, "PA10"))],
        )];
        let r = resolve(&uart_link(), &views, &[]);
        assert_eq!(r.a, Some((0, 0)));
        assert_eq!(r.b, None);
        assert_eq!(
            r.broken.as_deref(),
            Some("esp32_radio is not in the system")
        );
        let views = [
            chip(
                "stm32_main",
                vec![usart(1, 115_200, (30, "PA9"), (31, "PA10"))],
            ),
            chip("esp32_radio", vec![]),
        ];
        let r = resolve(&uart_link(), &views, &[]);
        assert_eq!(
            r.broken.as_deref(),
            Some("USART0 is no longer on esp32_radio")
        );
        assert!(r.wires.is_empty());
    }

    /// A linked module faces its partner's chip; the unlinked ones follow the
    /// majority; Detailed lists each pad once, even for a module in two links.
    #[test]
    fn modules_are_arranged_towards_their_partners() {
        use crate::panels::board::layout::Side;
        use eframe::egui::{pos2, vec2};
        let spi_a = spi_mod(1, SpiRole::Master, 0, false);
        let views = [
            chip(
                "mid",
                vec![usart(1, 115_200, (30, "PA9"), (31, "PA10")), spi_a],
            ),
            chip(
                "left",
                vec![usart(0, 115_200, (20, "GPIO21"), (21, "GPIO20"))],
            ),
            chip("right", vec![spi_mod(2, SpiRole::Slave, 0, false)]),
        ];
        let frames = [
            (pos2(500.0, 0.0), vec2(300.0, 200.0)),
            (pos2(0.0, 0.0), vec2(300.0, 200.0)),
            (pos2(1000.0, 0.0), vec2(300.0, 200.0)),
        ];
        let links = [
            Link {
                a: end("mid", ModuleKind::GenericInterfaceUsart, 1),
                b: end("left", ModuleKind::GenericInterfaceUsart, 0),
            },
            Link {
                a: end("mid", ModuleKind::GenericInterfaceSpi, 1),
                b: end("right", ModuleKind::GenericInterfaceSpi, 2),
            },
        ];
        let resolved: Vec<Resolved> = links.iter().map(|l| resolve(l, &views, &links)).collect();
        let a = arrange(&views, &frames, &resolved, false);
        assert_eq!(a[0].side, [Side::Left, Side::Right]);
        assert_eq!(a[1].side, [Side::Right]);
        assert_eq!(a[2].side, [Side::Left]);
        assert!(
            a.iter().all(|x| x.pins.iter().all(Vec::is_empty)),
            "Abstract: no pads"
        );

        let d = arrange(&views, &frames, &resolved, true);
        let labels: Vec<&str> = d[0].pins[0].iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(labels, ["TX PA9", "RX PA10"]);
        assert_eq!(d[0].pins[1].len(), 3, "SCK, MOSI, MISO");
    }

    /// Two single-wire ends share one DATA line - no "TX has no RX" - and a
    /// single-wire bus may join many ends; single-wire to two-wire is said.
    #[test]
    fn half_duplex_uarts_share_one_line() {
        let single = |instance: u8, tx: (usize, &str)| {
            let mut m = usart(instance, 115_200, tx, (99, "UNUSED"));
            if let ModuleConfig::Usart(c) = &mut m.config {
                c.direction = UsartDirection::HalfDuplexOnTx;
            }
            m.signals.retain(|s| s.signal == ModuleSignal::Tx);
            m
        };
        let views = [
            chip("stm32_main", vec![single(1, (30, "PA9"))]),
            chip("esp32_radio", vec![single(0, (20, "GPIO21"))]),
        ];
        let third = Link {
            a: end("stm32_main", ModuleKind::GenericInterfaceUsart, 1),
            b: end("pico", ModuleKind::GenericInterfaceUsart, 0),
        };
        let r = resolve(&uart_link(), &views, &[uart_link(), third]);
        assert_eq!(
            pads(&r),
            [("DATA PA9".into(), "DATA GPIO21".into(), Dir::Both)]
        );
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);

        let views = [
            chip("stm32_main", vec![single(1, (30, "PA9"))]),
            chip(
                "esp32_radio",
                vec![usart(0, 115_200, (20, "GPIO21"), (21, "GPIO20"))],
            ),
        ];
        let r = resolve(&uart_link(), &views, &[uart_link()]);
        assert!(r.wires.is_empty());
        assert!(r.warnings[0].contains("single-wire"), "{:?}", r.warnings);
    }

    /// A transmit-only end has no RX to be reached - no false warning for
    /// the direction it never uses.
    #[test]
    fn a_tx_only_uart_is_read_as_such() {
        let mut a = usart(1, 115_200, (30, "PA9"), (31, "PA10"));
        if let ModuleConfig::Usart(c) = &mut a.config {
            c.direction = UsartDirection::TxOnly;
        }
        let mut b = usart(0, 115_200, (20, "GPIO21"), (21, "GPIO20"));
        if let ModuleConfig::Usart(c) = &mut b.config {
            c.direction = UsartDirection::RxOnly;
        }
        let views = [chip("stm32_main", vec![a]), chip("esp32_radio", vec![b])];
        let r = resolve(&uart_link(), &views, &[uart_link()]);
        assert_eq!(pads(&r), [("TX PA9".into(), "RX GPIO20".into(), Dir::AtoB)]);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    /// A UART its chip's main.rs does not build has no line: the link says
    /// so instead of drawing one.
    #[test]
    fn a_uart_main_rs_does_not_build_draws_no_line() {
        let mut a = usart(1, 115_200, (30, "PA9"), (31, "PA10"));
        a.not_built = Some("its main.rs says why".to_owned());
        a.signals.clear();
        let b = usart(0, 115_200, (20, "GPIO21"), (21, "GPIO20"));
        let views = [chip("stm32_main", vec![a]), chip("esp32_radio", vec![b])];
        let r = resolve(&uart_link(), &views, &[uart_link()]);
        assert!(r.wires.is_empty());
        assert_eq!(
            r.warnings,
            ["stm32_main GenericInterfaceUsart1 is not built: its main.rs says why"]
        );
    }

    /// The wires are the firmware's (`ChipView::from_mcu`): two F1s with every
    /// pad wired, one on stm32f1xx-hal - the pair, CTS and RTS plain GPIOs -
    /// the other on embassy with RTS/CTS. Only that one's flow lines are
    /// drawn, and its CTS, which nothing clears, is said.
    #[test]
    fn flow_lines_are_the_ones_the_firmware_configures() {
        use crate::panels::board::snapshot::ChipView;
        use crate::panels::mcu_module::builtins::builtin_for;
        use crate::panels::mcu_module::mcu::Runtime;
        use crate::panels::mcu_module::modules::{UsartFlow, UsartMode};
        let f103 = |runtime: Runtime| {
            let mut mcu = builtin_for("stm32f103c8t6")
                .expect("built-in F103")
                .build_mcu();
            mcu.runtime = runtime;
            for (name, f) in [
                ("PA9", PinFunction::UsartTx(1)),
                ("PA10", PinFunction::UsartRx(1)),
                ("PA11", PinFunction::UsartCts(1)),
                ("PA12", PinFunction::UsartRts(1)),
            ] {
                mcu.iter_all_pins_mut()
                    .find(|p| p.name == name)
                    .expect(name)
                    .selected_function = f;
            }
            mcu.reconcile_modules();
            for m in &mut mcu.modules {
                if let ModuleConfig::Usart(c) = &mut m.config {
                    c.mode = UsartMode::Dma;
                    c.flow = UsartFlow::CtsRts;
                }
            }
            mcu
        };
        let views = [
            ChipView::from_mcu("hal", "STM32F103C8", &f103(Runtime::Blocking)),
            ChipView::from_mcu("embassy", "STM32F103C8", &f103(Runtime::Async)),
        ];
        let link = Link {
            a: end("hal", ModuleKind::GenericInterfaceUsart, 1),
            b: end("embassy", ModuleKind::GenericInterfaceUsart, 1),
        };
        let r = resolve(&link, &views, std::slice::from_ref(&link));
        assert_eq!(
            pads(&r),
            [
                ("TX PA9".into(), "RX PA10".into(), Dir::AtoB),
                ("RX PA10".into(), "TX PA9".into(), Dir::BtoA),
            ]
        );
        assert_eq!(
            r.warnings,
            ["embassy USART1 CTS has no RTS to clear it on hal USART1"]
        );
    }

    /// CAN without transceivers is pad to pad; with one on one side only it
    /// cannot talk.
    #[test]
    fn can_without_transceivers_wires_the_pads() {
        let can_mod = |transceiver: bool| {
            let mut c = CanModuleConfig::new(1);
            c.transceiver = transceiver;
            module(
                ModuleKind::GenericInterfaceCan,
                1,
                ModuleConfig::Can(c),
                vec![
                    sp(ModuleSignal::CanTx, 1, "GPIO4"),
                    sp(ModuleSignal::CanRx, 2, "GPIO5"),
                ],
            )
        };
        let link = Link {
            a: end("a", ModuleKind::GenericInterfaceCan, 1),
            b: end("b", ModuleKind::GenericInterfaceCan, 1),
        };
        let views = [
            chip("a", vec![can_mod(false)]),
            chip("b", vec![can_mod(false)]),
        ];
        let r = resolve(&link, &views, &[]);
        assert_eq!(
            pads(&r),
            [
                ("TX GPIO4".into(), "RX GPIO5".into(), Dir::AtoB),
                ("RX GPIO5".into(), "TX GPIO4".into(), Dir::BtoA),
            ]
        );
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let views = [
            chip("a", vec![can_mod(true)]),
            chip("b", vec![can_mod(false)]),
        ];
        let r = resolve(&link, &views, &[]);
        assert!(r.wires.is_empty());
        assert!(r.warnings[0].contains("cannot talk"), "{:?}", r.warnings);
    }

    /// A custom module's number handed to another module: the link does not
    /// quietly follow it.
    #[test]
    fn a_reused_custom_number_breaks_the_link() {
        use PinFunction::{GpioInput, GpioOutput};
        let mut a = custom(0, &[(1, "PB0", GpioInput)]);
        a.name = "irq_in".into();
        let mut b = custom(3, &[(9, "GPIO4", GpioOutput)]);
        b.name = "irq_out".into();
        let mut link = Link {
            a: end_for("a", &a),
            b: end_for("b", &b),
        };
        assert_eq!(link.b.name, "irq_out");
        let views = [chip("a", vec![a.clone()]), chip("b", vec![b.clone()])];
        assert_eq!(resolve(&link, &views, &[]).broken, None);
        b.name = "led".into();
        let views = [chip("a", vec![a]), chip("b", vec![b])];
        let r = resolve(&link, &views, &[]);
        assert!(r.broken.unwrap().contains("now belongs to led"));
        // A peripheral end carries no name, and needs none.
        link.a = end_for("a", &usart(1, 9600, (1, "X"), (2, "Y")));
        assert!(link.a.name.is_empty());
    }

    /// Two open-drain outputs are a wired-AND, not a fight.
    #[test]
    fn open_drain_outputs_share_a_line() {
        use PinFunction::GpioOutput;
        let mut a = custom(0, &[(1, "PB0", GpioOutput)]);
        let mut b = custom(1, &[(9, "PB1", GpioOutput)]);
        a.signals[0].open_drain = true;
        b.signals[0].open_drain = true;
        let link = Link {
            a: end("a", ModuleKind::Custom, 0),
            b: end("b", ModuleKind::Custom, 1),
        };
        let views = [chip("a", vec![a]), chip("b", vec![b])];
        let r = resolve(&link, &views, &[]);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert!(r.notes[0].contains("wired-AND"));
    }

    /// A chip's SPI master reaches an FPGA described as a part: the wires
    /// run to the part's own pin names, the clock from the chip, and a 1.8 V
    /// part on a 3.3 V chip is flagged.
    #[test]
    fn a_chip_links_to_an_external_part_like_to_a_chip() {
        use crate::panels::board::parts::{Part, PartBus};
        let mut fpga = Part::new("fpga");
        fpga.io_mv = 1800;
        fpga.add_interface(PartBus::SpiSlave);
        for (pin, name) in fpga.interfaces[0]
            .pins
            .iter_mut()
            .zip(["15", "17", "14", "16"])
        {
            pin.name = format!("pin {name}");
        }
        let views = [
            chip("a", vec![spi_mod(1, SpiRole::Master, 0, true)]),
            fpga.view(),
        ];
        let link = Link {
            a: end("a", ModuleKind::GenericInterfaceSpi, 1),
            b: end("fpga", ModuleKind::GenericInterfaceSpi, 0),
        };
        let r = resolve(&link, &views, &[]);
        assert_eq!(r.broken, None);
        let got: Vec<(String, Dir)> = r.wires.iter().map(|w| (w.b.label(), w.dir)).collect();
        assert_eq!(
            got,
            [
                ("SCK pin 15".into(), Dir::AtoB),
                ("MOSI pin 17".into(), Dir::AtoB),
                ("MISO pin 14".into(), Dir::BtoA),
                ("NSS pin 16".into(), Dir::AtoB),
            ]
        );
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("1.8 V"), "{:?}", r.warnings);
        fpga.io_mv = 3300;
        let views = [
            chip("a", vec![spi_mod(1, SpiRole::Master, 0, true)]),
            fpga.view(),
        ];
        assert!(resolve(&link, &views, &[]).warnings.is_empty());
    }

    /// An I2C part is a target: no "two controllers" warning, and no clock
    /// comparison - a target follows the controller's clock.
    #[test]
    fn an_i2c_part_is_a_target() {
        use crate::panels::board::parts::{Part, PartBus};
        let mut sensor = Part::new("bme");
        sensor.add_interface(PartBus::I2c);
        let mut c = I2cModuleConfig::new(1);
        c.clock_hz = 400_000;
        let chip_i2c = module(
            ModuleKind::GenericInterfaceI2c,
            1,
            ModuleConfig::I2c(c),
            vec![
                sp(ModuleSignal::Scl, 1, "PB6"),
                sp(ModuleSignal::Sda, 2, "PB7"),
            ],
        );
        let views = [chip("a", vec![chip_i2c]), sensor.view()];
        let link = Link {
            a: end("a", ModuleKind::GenericInterfaceI2c, 1),
            b: end("bme", ModuleKind::GenericInterfaceI2c, 0),
        };
        let r = resolve(&link, &views, &[]);
        assert_eq!(r.wires.len(), 2);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    /// Levels only matter where pins meet: CAN through transceivers to a 5 V
    /// part is fine; two I2C targets have nobody driving the clock.
    #[test]
    fn part_checks_follow_the_wires() {
        use crate::panels::board::parts::{Part, PartBus};
        let mut node = Part::new("node");
        node.io_mv = 5000;
        node.add_interface(PartBus::Can);
        let chip_can = module(
            ModuleKind::GenericInterfaceCan,
            1,
            ModuleConfig::Can(CanModuleConfig::new(1)),
            vec![
                sp(ModuleSignal::CanTx, 1, "PB9"),
                sp(ModuleSignal::CanRx, 2, "PB8"),
            ],
        );
        let views = [chip("a", vec![chip_can]), node.view()];
        let link = Link {
            a: end("a", ModuleKind::GenericInterfaceCan, 1),
            b: end("node", ModuleKind::GenericInterfaceCan, 0),
        };
        let r = resolve(&link, &views, &[]);
        assert!(r.wires.is_empty());
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);

        let mut s1 = Part::new("s1");
        s1.add_interface(PartBus::I2c);
        let mut s2 = Part::new("s2");
        s2.add_interface(PartBus::I2c);
        let views = [s1.view(), s2.view()];
        let link = Link {
            a: end("s1", ModuleKind::GenericInterfaceI2c, 0),
            b: end("s2", ModuleKind::GenericInterfaceI2c, 0),
        };
        let w = resolve(&link, &views, &[]).warnings;
        assert!(w.iter().any(|x| x.contains("both I2C targets")), "{w:?}");
    }

    /// Detailed lists every module's pads, with no link anywhere - a system
    /// of one chip must not look the same in both views.
    #[test]
    fn detailed_lists_the_pads_of_every_module() {
        let views = [chip(
            "solo",
            vec![
                usart(1, 115_200, (30, "PA9"), (31, "PA10")),
                spi_mod(2, SpiRole::Master, 0, false),
            ],
        )];
        let frames = [(
            eframe::egui::pos2(0.0, 0.0),
            eframe::egui::vec2(300.0, 200.0),
        )];
        let abstract_view = arrange(&views, &frames, &[], false);
        assert!(abstract_view[0].pins.iter().all(Vec::is_empty));
        let detailed = arrange(&views, &frames, &[], true);
        let labels: Vec<Vec<&str>> = detailed[0]
            .pins
            .iter()
            .map(|p| p.iter().map(|(_, l)| l.as_str()).collect())
            .collect();
        assert_eq!(
            labels,
            [
                vec!["TX PA9", "RX PA10"],
                vec!["SCK SCK", "MOSI MOSI", "MISO MISO"],
            ]
        );
    }

    #[test]
    fn only_modules_on_the_same_bus_can_be_linked() {
        use ModuleKind::*;
        assert!(can_link(GenericInterfaceUsart, GenericInterfaceLpuart));
        assert!(can_link(Custom, Custom));
        assert!(can_link(GenericInterfaceUsb, GenericInterfaceUsb));
        assert!(!can_link(GenericInterfaceUsart, GenericInterfaceSpi));
        assert!(!can_link(GenericInterfaceI2s, GenericInterfaceSai));
        assert!(!can_link(GenericInterfaceTimer, GenericInterfaceTimer));
    }
}
