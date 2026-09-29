//! Which DMA channel carries which peripheral, per STM32 family.
//!
//! embassy makes the CALLER name the channel *and* bind its interrupt, so a
//! generated project cannot build until both are filled in. Before this, both
//! were a `TODO` the user had to resolve by hand against the reference manual.
//!
//! # Why a table and not a lookup
//!
//! The truth lives in `stm32-metapac`, which the IDE does not depend on (it
//! generates code, it does not compile it). The data below was harvested from
//! embassy's own generated `dma_trait_impl!` lines for a real build, rather
//! than transcribed from a datasheet.
//!
//! # Two columns, not one
//!
//! A channel has a PERIPHERAL name and an INTERRUPT name, and they differ: on
//! STM32F4 `DMA2_CH7`'s interrupt is `DMA2_STREAM7`; on an L4-class part
//! `DMA1_CH4`'s is `DMA1_CHANNEL4`. Both appear in the generated code, in
//! different places, so [`DmaPick`] carries both.
//!
//! # Why an allocator and not a constant
//!
//! Most peripherals accept SEVERAL channels, and the sets overlap: on F4,
//! `SPI1_TX` may use DMA2 channel 2, 3 or 5 while `USART1_RX` may use 2 or 5.
//! Picking the first candidate for each would hand the same channel to both, so
//! callers walk their peripherals through one [`DmaAllocator`], which skips what
//! it has already given out.

use std::collections::BTreeSet;

/// A DMA channel, under both the names the generated code needs.
///
/// `Default` (both names empty) is the "this half is not used" placeholder a
/// one-way peripheral passes around — see `dma_args`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DmaPick {
    /// The `embassy_stm32::peripherals` singleton, e.g. `DMA2_CH7`.
    pub peri: String,
    /// The `bind_interrupts!` key, e.g. `DMA2_STREAM7`.
    pub irq: String,
}

/// One DMA channel a project actually uses, as REPORTED BY CODEGEN.
///
/// Not re-derived for the UI: the Configuration tab's list is filled from the
/// same pass that writes `main.rs`, so it cannot describe an allocation the
/// project does not have. A list that drifts from the code is worse than no
/// list — it is a wrong answer with a confident face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DmaUse {
    /// The channel singleton — `DMA1_CH4`, `GPDMA1_CH0`.
    pub peri: String,
    /// The `bind_interrupts!` key. Empty on the STM32F1 blocking path, where
    /// the HAL owns the interrupt and generated code never names it.
    pub irq: String,
    /// Who has it: `USART1 TX`, `SPI2 RX`.
    pub user: String,
    /// Pinned by hand in the Virtual Module rather than allocated.
    pub manual: bool,
}

/// Bus kinds that can run on DMA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bus {
    Usart,
    /// A peripheral of its own, not a USART instance — its DMA requests are
    /// named `LPUART1_TX/RX`, so it cannot share [`Bus::Usart`]'s lookup.
    Lpuart,
    Spi,
    I2c,
    /// The SD-card controller. ST gives it ONE bidirectional request, so the
    /// direction is ignored here.
    Sdmmc,
}

impl Bus {
    /// How the bus is named in a channel's "used by" line.
    pub fn label(self) -> &'static str {
        match self {
            Bus::Usart => "USART",
            Bus::Lpuart => "LPUART",
            Bus::Spi => "SPI",
            Bus::I2c => "I2C",
            Bus::Sdmmc => "SDMMC",
        }
    }
}

/// Transfer direction - a peripheral's TX and RX channels are different.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Tx,
    Rx,
}

/// The F4 request map, harvested from embassy's generated `dma_trait_impl!`
/// lines for an STM32F411RE and **verified by a real cross-compile** (see
/// `emit_embassy_project`). It is the family's DMA request table, not a
/// per-part quirk, so it holds for the other F4 parts that have the same
/// peripheral - an instance a part lacks simply never gets asked for.
///
/// Ordered by preference: the first candidate still free wins.
const F4: &[(Bus, u8, Dir, &[&str])] = &[
    (Bus::Usart, 1, Dir::Tx, &["DMA2_CH7"]),
    (Bus::Usart, 1, Dir::Rx, &["DMA2_CH5", "DMA2_CH2"]),
    (Bus::Usart, 2, Dir::Tx, &["DMA1_CH6"]),
    (Bus::Usart, 2, Dir::Rx, &["DMA1_CH5", "DMA1_CH7"]),
    (Bus::Usart, 6, Dir::Tx, &["DMA2_CH6", "DMA2_CH7"]),
    (Bus::Usart, 6, Dir::Rx, &["DMA2_CH1", "DMA2_CH2"]),
    (Bus::Spi, 1, Dir::Tx, &["DMA2_CH3", "DMA2_CH5", "DMA2_CH2"]),
    (Bus::Spi, 1, Dir::Rx, &["DMA2_CH0", "DMA2_CH2"]),
    (Bus::Spi, 2, Dir::Tx, &["DMA1_CH4"]),
    (Bus::Spi, 2, Dir::Rx, &["DMA1_CH3"]),
    (Bus::Spi, 3, Dir::Tx, &["DMA1_CH5", "DMA1_CH7"]),
    (Bus::Spi, 3, Dir::Rx, &["DMA1_CH0", "DMA1_CH2"]),
    (Bus::I2c, 1, Dir::Tx, &["DMA1_CH6", "DMA1_CH7", "DMA1_CH1"]),
    (Bus::I2c, 1, Dir::Rx, &["DMA1_CH0", "DMA1_CH5"]),
    (Bus::I2c, 2, Dir::Tx, &["DMA1_CH7"]),
    (Bus::I2c, 2, Dir::Rx, &["DMA1_CH2", "DMA1_CH3"]),
    (Bus::I2c, 3, Dir::Tx, &["DMA1_CH4", "DMA1_CH5"]),
    (Bus::I2c, 3, Dir::Rx, &["DMA1_CH1", "DMA1_CH2"]),
];

/// The F2 request map, harvested the same way from an STM32F217ZE build and
/// **verified by a real cross-compile**.
///
/// Close to [`F4`] but NOT the same, which is why it is its own table rather
/// than a shared one: USART2's RX has a single channel here where F4 offers two,
/// SPI1's TX drops F4's third option, I2C1's TX drops one and I2C3 is down to a
/// single channel each way. F2 also has USART3, which the F411 the F4 table came
/// from does not.
const F2: &[(Bus, u8, Dir, &[&str])] = &[
    (Bus::Usart, 1, Dir::Tx, &["DMA2_CH7"]),
    (Bus::Usart, 1, Dir::Rx, &["DMA2_CH5", "DMA2_CH2"]),
    (Bus::Usart, 2, Dir::Tx, &["DMA1_CH6"]),
    (Bus::Usart, 2, Dir::Rx, &["DMA1_CH5"]),
    (Bus::Usart, 3, Dir::Tx, &["DMA1_CH3", "DMA1_CH4"]),
    (Bus::Usart, 3, Dir::Rx, &["DMA1_CH1"]),
    (Bus::Usart, 6, Dir::Tx, &["DMA2_CH6", "DMA2_CH7"]),
    (Bus::Usart, 6, Dir::Rx, &["DMA2_CH1", "DMA2_CH2"]),
    (Bus::Spi, 1, Dir::Tx, &["DMA2_CH3", "DMA2_CH5"]),
    (Bus::Spi, 1, Dir::Rx, &["DMA2_CH0", "DMA2_CH2"]),
    (Bus::Spi, 2, Dir::Tx, &["DMA1_CH4"]),
    (Bus::Spi, 2, Dir::Rx, &["DMA1_CH3"]),
    (Bus::Spi, 3, Dir::Tx, &["DMA1_CH5", "DMA1_CH7"]),
    (Bus::Spi, 3, Dir::Rx, &["DMA1_CH0", "DMA1_CH2"]),
    (Bus::I2c, 1, Dir::Tx, &["DMA1_CH6", "DMA1_CH7"]),
    (Bus::I2c, 1, Dir::Rx, &["DMA1_CH0", "DMA1_CH5"]),
    (Bus::I2c, 2, Dir::Tx, &["DMA1_CH7"]),
    (Bus::I2c, 2, Dir::Rx, &["DMA1_CH2", "DMA1_CH3"]),
    (Bus::I2c, 3, Dir::Tx, &["DMA1_CH4"]),
    (Bus::I2c, 3, Dir::Rx, &["DMA1_CH2"]),
];

/// The F7 request map, harvested from an STM32F767ZI build and **verified by a
/// real cross-compile** on an STM32F746ZG - the two were diffed over every
/// instance below and are identical, which is what makes one table per family
/// defensible here rather than one per part.
///
/// USART, SPI1/3 and I2C1 match [`F2`]; the differences are all extra room:
/// SPI2 gains a second channel each way, I2C2's TX and I2C3 both directions
/// gain one. Nothing F2 offers is missing, but the reverse is not true, so the
/// tables stay separate.
const F7: &[(Bus, u8, Dir, &[&str])] = &[
    (Bus::Usart, 1, Dir::Tx, &["DMA2_CH7"]),
    (Bus::Usart, 1, Dir::Rx, &["DMA2_CH5", "DMA2_CH2"]),
    (Bus::Usart, 2, Dir::Tx, &["DMA1_CH6"]),
    (Bus::Usart, 2, Dir::Rx, &["DMA1_CH5"]),
    (Bus::Usart, 3, Dir::Tx, &["DMA1_CH3", "DMA1_CH4"]),
    (Bus::Usart, 3, Dir::Rx, &["DMA1_CH1"]),
    (Bus::Usart, 6, Dir::Tx, &["DMA2_CH6", "DMA2_CH7"]),
    (Bus::Usart, 6, Dir::Rx, &["DMA2_CH1", "DMA2_CH2"]),
    (Bus::Spi, 1, Dir::Tx, &["DMA2_CH3", "DMA2_CH5"]),
    (Bus::Spi, 1, Dir::Rx, &["DMA2_CH0", "DMA2_CH2"]),
    (Bus::Spi, 2, Dir::Tx, &["DMA1_CH4", "DMA1_CH6"]),
    (Bus::Spi, 2, Dir::Rx, &["DMA1_CH3", "DMA1_CH1"]),
    (Bus::Spi, 3, Dir::Tx, &["DMA1_CH5", "DMA1_CH7"]),
    (Bus::Spi, 3, Dir::Rx, &["DMA1_CH0", "DMA1_CH2"]),
    (Bus::I2c, 1, Dir::Tx, &["DMA1_CH6", "DMA1_CH7"]),
    (Bus::I2c, 1, Dir::Rx, &["DMA1_CH0", "DMA1_CH5"]),
    (Bus::I2c, 2, Dir::Tx, &["DMA1_CH7", "DMA1_CH4"]),
    (Bus::I2c, 2, Dir::Rx, &["DMA1_CH2", "DMA1_CH3"]),
    (Bus::I2c, 3, Dir::Tx, &["DMA1_CH4", "DMA1_CH0"]),
    (Bus::I2c, 3, Dir::Rx, &["DMA1_CH2", "DMA1_CH1"]),
];

/// The F1 request map, harvested from embassy-stm32 0.6's generated
/// `dma_trait_impl!` lines for an STM32F103C8 build (stm32-metapac 21) and
/// cross-compiled there. Only on the Async runtime: the Blocking F1 path runs
/// on stm32f1xx-hal, which fixes each channel in its own types.
///
/// No request mux and one channel per request, so every list is a single
/// entry - and two peripherals wanting the same channel is a real collision
/// (USART1 and SPI2 both need CH4/CH5), which the allocator reports as such.
/// Only DMA1: the high-density parts' DMA2 has not been harvested.
const F1: &[(Bus, u8, Dir, &[&str])] = &[
    (Bus::Usart, 1, Dir::Tx, &["DMA1_CH4"]),
    (Bus::Usart, 1, Dir::Rx, &["DMA1_CH5"]),
    (Bus::Usart, 2, Dir::Tx, &["DMA1_CH7"]),
    (Bus::Usart, 2, Dir::Rx, &["DMA1_CH6"]),
    (Bus::Usart, 3, Dir::Tx, &["DMA1_CH2"]),
    (Bus::Usart, 3, Dir::Rx, &["DMA1_CH3"]),
    (Bus::Spi, 1, Dir::Tx, &["DMA1_CH3"]),
    (Bus::Spi, 1, Dir::Rx, &["DMA1_CH2"]),
    (Bus::Spi, 2, Dir::Tx, &["DMA1_CH5"]),
    (Bus::Spi, 2, Dir::Rx, &["DMA1_CH4"]),
    (Bus::I2c, 1, Dir::Tx, &["DMA1_CH6"]),
    (Bus::I2c, 1, Dir::Rx, &["DMA1_CH7"]),
    (Bus::I2c, 2, Dir::Tx, &["DMA1_CH4"]),
    (Bus::I2c, 2, Dir::Rx, &["DMA1_CH5"]),
];

impl Dir {
    pub fn label(self) -> &'static str {
        match self {
            Dir::Tx => "TX",
            Dir::Rx => "RX",
        }
    }
}

/// The candidate channels for one peripheral direction, best first.
///
/// `None` for a family with no table yet - the caller then leaves its `TODO` in
/// place, which is what every family did before this existed. Guessing would
/// produce code that compiles and moves the wrong bytes.
fn candidates(family: &str, bus: Bus, instance: u8, dir: Dir) -> Option<&'static [&'static str]> {
    let table = match family {
        "stm32f4" => F4,
        "stm32f2" => F2,
        "stm32f7" => F7,
        "stm32f1" => F1,
        // Every other family keeps the TODO. The tables here differ from
        // each other in ways no amount of squinting at the reference manuals
        // would have predicted, so a new one means harvesting and compiling it,
        // not extending a pattern.
        _ => return None,
    };
    table
        .iter()
        .find(|(b, i, d, _)| *b == bus && *i == instance && *d == dir)
        .map(|(_, _, _, c)| *c)
}

/// The `bind_interrupts!` key for a channel peripheral name.
///
/// STM32F4 calls the peripheral `DMA2_CH7` and its interrupt `DMA2_STREAM7`
/// (ST's "stream" naming, which embassy keeps for the vector but not for the
/// singleton). Verified against embassy's `dma_channel_impl!` output.
fn irq_for(family: &str, channel: &str) -> Option<String> {
    match family {
        // Both use ST's "stream" naming for the vector while embassy keeps
        // "channel" for the singleton. Verified against `dma_channel_impl!`
        // output for an F411 and an F217ZE.
        "stm32f4" | "stm32f2" | "stm32f7" => Some(channel.replace("_CH", "_STREAM")),
        // The F1 has channels, not streams, and names the vector in full:
        // `DMA1_CH4` is `DMA1_CHANNEL4`. Compiled on an STM32F103C8.
        "stm32f1" => Some(channel.replace("_CH", "_CHANNEL")),
        _ => None,
    }
}

/// Hands out DMA channels for one project, never the same one twice.
///
/// Two ways to answer, in order:
///
/// 1. **The chip's own data**, when it was imported from the vendor database.
///    Muxed (DMAMUX / GPDMA, 1367 of the database's 2123 parts): any free
///    channel, because that is what muxed means. Fixed mapping (756 parts):
///    only the channels its request table routes this peripheral to.
/// 2. **The family table** above, for chips carrying no vendor data — the two
///    built-ins, and anything imported from the public open-pin-data repo.
///
/// A chip that is neither still answers `None`, and the caller keeps its
/// `TODO`. Guessing would produce code that compiles and moves the wrong bytes.
/// Why [`DmaAllocator::take_named_or`] would not hand over a named channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedRefusal {
    /// Another peripheral in this project already has it - two Virtual Modules
    /// pinned to the same channel. The user can see this one and undo it.
    AlreadyTaken,
    /// No interrupt name is known for it, so `bind_interrupts!` would not
    /// compile. A fact about the chip, not a mistake.
    UnknownIrq,
}

#[derive(Debug, Default)]
pub struct DmaAllocator {
    family: String,
    used: BTreeSet<String>,
    /// Channels already HANDED OUT, as opposed to merely out of circulation.
    ///
    /// `used` answers "may the allocator pick this?", and a hand-pinned channel
    /// enters it BEFORE anything is allocated so nothing else takes it. That
    /// makes `used` useless for the second question - "has this already been
    /// GIVEN to a peripheral?" - because a pinned channel is in it before its
    /// own owner asks for it.
    ///
    /// Without the distinction, two modules pinned to one channel were both
    /// handed it: `channels_for` offers every channel without excluding the
    /// ones other modules already name, so pinning `DMA1_CH4` on USART1 TX and
    /// again on SPI1 TX emitted it twice.
    claimed: BTreeSet<String>,
    /// What the vendor database says about THIS chip. `None` for a built-in
    /// chip or one imported before the data was captured.
    chip: Option<crate::panels::mcu_module::mcu_def::DmaDef>,
}

impl DmaAllocator {
    pub fn new(family: &str) -> Self {
        Self {
            family: family.to_owned(),
            used: BTreeSet::new(),
            claimed: BTreeSet::new(),
            chip: None,
        }
    }

    /// The allocator for a specific chip — its own channels and request table
    /// first, the family tables as the fallback.
    pub fn for_chip(
        family: &str,
        dma: Option<&crate::panels::mcu_module::mcu_def::DmaDef>,
    ) -> Self {
        Self {
            family: family.to_owned(),
            used: BTreeSet::new(),
            claimed: BTreeSet::new(),
            chip: dma.cloned(),
        }
    }

    /// The channel for `bus{instance}`'s `dir`, or `None` when nothing is known
    /// about the chip and the family has no table, or every candidate is
    /// already taken by an earlier peripheral.
    ///
    /// `None` is a normal outcome, not an error: the caller falls back to the
    /// `TODO` placeholder, so a project with more DMA buses than channels still
    /// generates - it just asks the user to finish it.
    pub fn take(&mut self, bus: Bus, instance: u8, dir: Dir) -> Option<DmaPick> {
        let pick = self
            .take_from_chip(bus, instance, dir)
            .or_else(|| self.take_from_family(bus, instance, dir))?;
        self.used.insert(pick.peri.clone());
        self.claimed.insert(pick.peri.clone());
        Some(pick)
    }

    /// Take a channel the USER named, rather than allocating one.
    ///
    /// The name comes from a Virtual Module's `dma_tx` / `dma_rx`, which the UI
    /// fills from [`channels_for`] — so it is normally one of the channels this
    /// peripheral can actually use. It is honoured even when it is not: the
    /// point of the field is to override the IDE, and a hand-edited
    /// `mcu.config` is the user's business. What CANNOT be honoured is a
    /// channel whose interrupt is unknown, because the binding would not
    /// compile.
    /// A channel already handed to another peripheral is refused, and the
    /// caller falls back to its `TODO` - the same outcome as any other channel
    /// it cannot resolve. Silently emitting it twice was worse than failing:
    /// embassy moves a `Peri`, so the SECOND use is the one that does not
    /// compile, and the error names a line the user never edited.
    pub fn take_named(&mut self, peri: &str) -> Option<DmaPick> {
        self.take_named_or(peri).ok()
    }

    /// As [`Self::take_named`], but saying WHY it refused.
    ///
    /// The two reasons want different words in the generated `TODO`: one is a
    /// mistake the user can see and undo, the other is a fact about the chip.
    pub fn take_named_or(&mut self, peri: &str) -> Result<DmaPick, NamedRefusal> {
        if self.claimed.contains(peri) {
            return Err(NamedRefusal::AlreadyTaken);
        }
        let irq = self
            .chip
            .as_ref()
            .and_then(|c| c.channels.iter().find(|c| c.peri == peri))
            .map(|c| c.irq.clone())
            .or_else(|| irq_for(&self.family, peri))
            .ok_or(NamedRefusal::UnknownIrq)?;
        self.used.insert(peri.to_owned());
        self.claimed.insert(peri.to_owned());
        Ok(DmaPick {
            peri: peri.to_owned(),
            irq,
        })
    }

    /// Whether this channel has already been handed to a peripheral.
    pub fn is_claimed(&self, peri: &str) -> bool {
        self.claimed.contains(peri)
    }

    /// Put a channel out of circulation without emitting anything for it.
    ///
    /// Every hand-picked channel in the project is reserved BEFORE the first
    /// automatic allocation, so an earlier peripheral cannot take the channel a
    /// later one was pinned to. Order of appearance would otherwise decide it,
    /// which is exactly the arbitrariness the manual field exists to remove.
    pub fn reserve(&mut self, peri: &str) {
        if !peri.is_empty() {
            self.used.insert(peri.to_owned());
        }
    }

    /// The vendor's own answer for this chip.
    fn take_from_chip(&self, bus: Bus, instance: u8, dir: Dir) -> Option<DmaPick> {
        let chip = self.chip.as_ref()?;
        let free = |peri: &str| !self.used.contains(peri);
        let peri = if chip.mux {
            // Muxed: the bus, the instance and the direction do not enter into
            // it. Any free channel carries any request - that is what the mux
            // is for.
            chip.channels
                .iter()
                .map(|c| c.peri.clone())
                .find(|p| free(p))?
        } else {
            // Fixed mapping: only the channels silicon routes this request to.
            let cands = request_names(bus, instance, dir)
                .into_iter()
                .find_map(|r| chip.requests.iter().find(|(n, _)| *n == r))
                .map(|(_, cs)| cs)?;
            cands.iter().find(|p| free(p)).cloned()?
        };
        // The interrupt is NOT derived from the channel name - `DMA2_CH7`'s is
        // `DMA2_STREAM7` on an F4 and `DMA2_CHANNEL7` elsewhere, and on an
        // STM32G0 several channels share one. The chip's own list has it.
        let irq = chip
            .channels
            .iter()
            .find(|c| c.peri == peri)
            .map(|c| c.irq.clone())?;
        Some(DmaPick { peri, irq })
    }

    /// The hand-harvested family table, for chips carrying no vendor data.
    fn take_from_family(&self, bus: Bus, instance: u8, dir: Dir) -> Option<DmaPick> {
        let cands = candidates(&self.family, bus, instance, dir)?;
        let chan = cands.iter().find(|c| !self.used.contains(**c))?;
        Some(DmaPick {
            peri: (*chan).to_owned(),
            irq: irq_for(&self.family, chan)?,
        })
    }
}

/// Every channel `bus{instance}`'s `dir` may use on this chip, for the picker.
///
/// Muxed chip: all of them, in vendor order. Fixed mapping: only what the
/// request table routes. Empty when the chip carries no vendor data at all —
/// the picker then offers nothing to choose, which is honest: without the data
/// the IDE cannot say which channels are valid, and a free-text field would
/// invite a name that silently moves the wrong bytes.
pub fn channels_for(
    dma: Option<&crate::panels::mcu_module::mcu_def::DmaDef>,
    bus: Bus,
    instance: u8,
    dir: Dir,
) -> Vec<String> {
    let Some(chip) = dma else {
        return Vec::new();
    };
    if chip.mux {
        return chip.channels.iter().map(|c| c.peri.clone()).collect();
    }
    request_names(bus, instance, dir)
        .into_iter()
        .find_map(|r| chip.requests.iter().find(|(n, _)| *n == r))
        .map(|(_, cs)| cs.clone())
        .unwrap_or_default()
}

/// What the vendor calls this request, best guess first.
///
/// ST names a UART's request after the peripheral, and an instance that is a
/// plain UART rather than a USART is spelled `UART4_TX` — while the IDE models
/// both as [`Bus::Usart`], because the pin functions do.
fn request_names(bus: Bus, instance: u8, dir: Dir) -> Vec<String> {
    let d = match dir {
        Dir::Tx => "TX",
        Dir::Rx => "RX",
    };
    match bus {
        // One request, both directions, and named after the block — `SDIO`
        // on the families that call it that.
        Bus::Sdmmc => vec![format!("SDMMC{instance}"), "SDIO".to_owned()],
        Bus::Usart => vec![
            format!("USART{instance}_{d}"),
            format!("UART{instance}_{d}"),
        ],
        Bus::Lpuart => vec![format!("LPUART{instance}_{d}")],
        Bus::Spi => vec![format!("SPI{instance}_{d}")],
        Bus::I2c => vec![format!("I2C{instance}_{d}")],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::panels::mcu_module::codegen::dma_data::DmaChannel;
    use crate::panels::mcu_module::mcu_def::DmaDef;

    fn chan(peri: &str, irq: &str) -> DmaChannel {
        DmaChannel {
            peri: peri.into(),
            irq: irq.into(),
        }
    }

    /// The three hand-harvested tables against the vendor database.
    ///
    /// [`F4`], [`F2`] and [`F7`] were transcribed from embassy's own generated
    /// `dma_trait_impl!` lines. The database is ST's source for the same fact,
    /// reached by a completely different route. If they agree, the extraction
    /// in [`super::dma_data::requests_from_modes`] is right — and the same
    /// extraction then covers the ~750 fixed-mapping parts nobody harvested.
    ///
    /// Ignored: needs the database on disk.
    ///
    /// ```text
    /// cargo test --bin rust_on_chip the_hand_tables_match -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs the STM32Cube database on disk"]
    fn the_hand_tables_match_the_vendor_database() {
        let Some(db) = super::super::dma_data::vendor_db_dir() else {
            eprintln!("no STM32Cube database found - nothing checked");
            return;
        };
        let mut cache = std::collections::HashMap::new();
        let mut checked = 0usize;
        for (family, chip, table) in [
            ("stm32f4", "STM32F411R(C-E)Tx.xml", F4),
            ("stm32f2", "STM32F217Z(E-G)Tx.xml", F2),
            ("stm32f7", "STM32F767ZITx.xml", F7),
            ("stm32f1", "STM32F103C(8-B)Tx.xml", F1),
        ] {
            let path = db.join(chip);
            let Ok(xml) = std::fs::read_to_string(&path) else {
                eprintln!("{chip} not in the database - skipped");
                continue;
            };
            let def = super::super::dma_data::dma_def_for(&xml, path.parent(), &mut cache)
                .unwrap_or_else(|| panic!("{chip} has no DMA data"));
            assert!(!def.mux, "{chip} should be a fixed-mapping part");
            for (bus, n, dir, hand) in table {
                let names = request_names(*bus, *n, *dir);
                let Some((_, db_chans)) = names
                    .iter()
                    .find_map(|r| def.requests.iter().find(|(k, _)| k == r))
                else {
                    panic!("{chip}: the database has no request {names:?}");
                };
                let mut want: Vec<&str> = hand.to_vec();
                want.sort_unstable();
                let mut got: Vec<&str> = db_chans.iter().map(String::as_str).collect();
                got.sort_unstable();
                assert_eq!(
                    want, got,
                    "{family} {bus:?}{n} {dir:?}: hand table {want:?}, database {got:?}"
                );
                checked += 1;
            }
        }
        // A green run that checked NOTHING is the failure this assert exists
        // for: a missing chip file is an `eprintln!` + `continue`, so when the
        // database moved out from under the old hardcoded path all three chips
        // skipped and the test still reported ok.
        assert!(
            checked > 0,
            "the database at {} yielded no comparable entries -              the guard would have passed while verifying nothing",
            db.display()
        );
        println!("{checked} hand-written entries confirmed against the database");
    }

    /// The whole point of the mux rule: a family with no table at all - here
    /// STM32G4 - now generates real channels, taken in order and never reused.
    #[test]
    fn a_muxed_chip_hands_out_its_own_channels_in_order() {
        let def = DmaDef {
            mux: true,
            channels: vec![
                chan("DMA1_CH1", "DMA1_CHANNEL1"),
                chan("DMA1_CH2", "DMA1_CHANNEL2"),
                chan("DMA1_CH3", "DMA1_CHANNEL3"),
            ],
            requests: Vec::new(),
        };
        let mut a = DmaAllocator::for_chip("stm32g4", Some(&def));
        let tx = a.take(Bus::Usart, 1, Dir::Tx).expect("USART1 TX");
        let rx = a.take(Bus::Usart, 1, Dir::Rx).expect("USART1 RX");
        assert_eq!(
            (tx.peri.as_str(), tx.irq.as_str()),
            ("DMA1_CH1", "DMA1_CHANNEL1")
        );
        assert_eq!(rx.peri, "DMA1_CH2");
        // A different peripheral keeps taking from the same pool.
        assert_eq!(a.take(Bus::Spi, 1, Dir::Tx).unwrap().peri, "DMA1_CH3");
        // Exhausted: back to the TODO rather than a channel already in use.
        assert_eq!(a.take(Bus::Spi, 1, Dir::Rx), None);
    }

    /// A chip with a FIXED request map is served from its request TABLE, never
    /// from its channel list: which channel carries which request is exactly
    /// what the hardware decides there.
    #[test]
    fn a_classic_chip_is_served_from_its_request_table() {
        let def = DmaDef {
            mux: false,
            channels: vec![
                chan("DMA1_CH2", "DMA1_CHANNEL2"),
                chan("DMA1_CH4", "DMA1_CHANNEL4"),
                chan("DMA1_CH5", "DMA1_CHANNEL5"),
                chan("DMA1_CH7", "DMA1_CHANNEL7"),
            ],
            requests: vec![
                ("USART1_TX".into(), vec!["DMA1_CH4".into()]),
                ("USART1_RX".into(), vec!["DMA1_CH5".into()]),
                ("UART4_TX".into(), vec!["DMA1_CH2".into()]),
                ("SPI1_TX".into(), vec!["DMA1_CH4".into(), "DMA1_CH7".into()]),
            ],
        };
        // An STM32L4 - a classic family with NO hand-written table, which used
        // to mean a TODO whatever the user did.
        let mut a = DmaAllocator::for_chip("stm32l4", Some(&def));
        let tx = a.take(Bus::Usart, 1, Dir::Tx).expect("USART1 TX");
        assert_eq!(
            (tx.peri.as_str(), tx.irq.as_str()),
            ("DMA1_CH4", "DMA1_CHANNEL4")
        );
        assert_eq!(a.take(Bus::Usart, 1, Dir::Rx).unwrap().peri, "DMA1_CH5");
        // CH4 is spoken for, so SPI1 TX falls to its second candidate rather
        // than sharing it.
        assert_eq!(a.take(Bus::Spi, 1, Dir::Tx).unwrap().peri, "DMA1_CH7");
        // A request the silicon does not route is not invented.
        assert_eq!(a.take(Bus::I2c, 1, Dir::Tx), None);
        // ST spells a plain UART's request `UART4_TX`; the IDE models it as the
        // same bus kind, so both names are tried.
        assert_eq!(
            DmaAllocator::for_chip("stm32l4", Some(&def))
                .take(Bus::Usart, 4, Dir::Tx)
                .unwrap()
                .peri,
            "DMA1_CH2"
        );
    }

    /// A channel pinned by hand in a Virtual Module: honoured, and taken out
    /// of circulation BEFORE anything is allocated, so whichever peripheral
    /// happens to be emitted first cannot claim it.
    /// Two Virtual Modules pinned to ONE channel.
    ///
    /// `reserve` puts a hand-pinned channel out of circulation before anything
    /// is allocated, so `used` says "taken" about a channel whose own owner has
    /// not asked for it yet. That made `used` unable to answer the second
    /// question - has this been HANDED OUT - and both peripherals got it. The
    /// generated code then moved the same `Peri` twice, and the error landed on
    /// the second peripheral's line, which the user never edited.
    #[test]
    fn one_channel_is_never_handed_to_two_peripherals() {
        let def = DmaDef {
            mux: true,
            channels: vec![
                chan("DMA1_CH1", "DMA1_CHANNEL1"),
                chan("DMA1_CH2", "DMA1_CHANNEL2"),
            ],
            requests: Vec::new(),
        };
        let mut a = DmaAllocator::for_chip("stm32g4", Some(&def));
        // Both modules name CH1, so it is reserved once and asked for twice.
        a.reserve("DMA1_CH1");
        a.reserve("DMA1_CH1");

        assert_eq!(
            a.take_named("DMA1_CH1").expect("the first owner").peri,
            "DMA1_CH1"
        );
        assert_eq!(
            a.take_named_or("DMA1_CH1"),
            Err(NamedRefusal::AlreadyTaken),
            "the second owner must be refused, not handed the same channel"
        );

        // And the refusal is specific: an unknown channel is a different fact.
        assert_eq!(
            a.take_named_or("DMA9_CH9"),
            Err(NamedRefusal::UnknownIrq),
            "no interrupt is known for it, which is not the user's mistake"
        );
    }

    /// Automatic allocation claims too, so a pinned channel cannot be taken by
    /// a peripheral that was allocated one earlier.
    #[test]
    fn an_automatically_allocated_channel_is_claimed_as_well() {
        let def = DmaDef {
            mux: true,
            channels: vec![chan("DMA1_CH1", "DMA1_CHANNEL1")],
            requests: Vec::new(),
        };
        let mut a = DmaAllocator::for_chip("stm32g4", Some(&def));
        assert_eq!(a.take(Bus::Usart, 1, Dir::Tx).unwrap().peri, "DMA1_CH1");
        assert!(a.is_claimed("DMA1_CH1"));
        assert_eq!(
            a.take_named_or("DMA1_CH1"),
            Err(NamedRefusal::AlreadyTaken),
            "pinning a channel the allocator already gave away must fail"
        );
    }

    #[test]
    fn a_reserved_channel_is_kept_for_whoever_pinned_it() {
        let def = DmaDef {
            mux: true,
            channels: vec![
                chan("DMA1_CH1", "DMA1_CHANNEL1"),
                chan("DMA1_CH2", "DMA1_CHANNEL2"),
                chan("DMA1_CH3", "DMA1_CHANNEL3"),
            ],
            requests: Vec::new(),
        };
        let mut a = DmaAllocator::for_chip("stm32g4", Some(&def));
        // SPI1 RX is pinned to CH1 - the very channel automatic allocation
        // would otherwise hand to the USART emitted before it.
        a.reserve("DMA1_CH1");
        assert_eq!(a.take(Bus::Usart, 1, Dir::Tx).unwrap().peri, "DMA1_CH2");
        assert_eq!(a.take(Bus::Usart, 1, Dir::Rx).unwrap().peri, "DMA1_CH3");
        let pinned = a.take_named("DMA1_CH1").expect("the pinned channel");
        assert_eq!(
            (pinned.peri.as_str(), pinned.irq.as_str()),
            ("DMA1_CH1", "DMA1_CHANNEL1"),
            "the interrupt comes from the chip's list, not from the name"
        );
    }

    /// The picker only ever offers channels the chip can really use.
    #[test]
    fn the_picker_offers_what_the_chip_allows() {
        let mux = DmaDef {
            mux: true,
            channels: vec![chan("DMA1_CH1", "i1"), chan("DMA1_CH2", "i2")],
            requests: Vec::new(),
        };
        assert_eq!(
            channels_for(Some(&mux), Bus::I2c, 1, Dir::Tx),
            ["DMA1_CH1", "DMA1_CH2"],
            "muxed: every channel, whatever the peripheral"
        );
        let classic = DmaDef {
            mux: false,
            channels: vec![chan("DMA1_CH4", "i4")],
            requests: vec![("USART1_TX".into(), vec!["DMA1_CH4".into()])],
        };
        assert_eq!(
            channels_for(Some(&classic), Bus::Usart, 1, Dir::Tx),
            ["DMA1_CH4"]
        );
        assert!(
            channels_for(Some(&classic), Bus::Spi, 1, Dir::Tx).is_empty(),
            "a request the silicon does not route offers nothing"
        );
        assert!(
            channels_for(None, Bus::Usart, 1, Dir::Tx).is_empty(),
            "no chip data, nothing to choose from"
        );
    }

    /// A channel the chip's NVIC list does not name has no interrupt, and an
    /// interrupt name cannot be guessed from the channel - so nothing is
    /// emitted rather than something unbuildable.
    #[test]
    fn a_channel_without_an_interrupt_is_not_offered() {
        let def = DmaDef {
            mux: false,
            channels: vec![chan("DMA1_CH1", "DMA1_CHANNEL1")],
            requests: vec![("USART1_TX".into(), vec!["DMA1_CH4".into()])],
        };
        let mut a = DmaAllocator::for_chip("stm32l4", Some(&def));
        assert_eq!(a.take(Bus::Usart, 1, Dir::Tx), None);
    }

    /// The vendor's table beats the hand-written one for the SAME family: it is
    /// per-chip, and the hand-written tables are per-family approximations.
    #[test]
    fn chip_data_wins_over_the_family_table() {
        let def = DmaDef {
            mux: false,
            channels: vec![chan("DMA1_CH3", "DMA1_CHANNEL3")],
            requests: vec![("USART1_TX".into(), vec!["DMA1_CH3".into()])],
        };
        let mut f4 = DmaAllocator::for_chip("stm32f4", Some(&def));
        let tx = f4.take(Bus::Usart, 1, Dir::Tx).unwrap();
        assert_eq!(tx.peri, "DMA1_CH3", "the F4 table would have said DMA2_CH7");
        // ...and a chip with no data at all still gets the family table.
        let mut bare = DmaAllocator::new("stm32f4");
        assert_eq!(bare.take(Bus::Usart, 1, Dir::Tx).unwrap().peri, "DMA2_CH7");
    }

    #[test]
    fn a_family_without_a_table_picks_nothing() {
        // Silence is the contract: the caller keeps its TODO rather than
        // receiving a plausible-looking guess.
        let mut a = DmaAllocator::new("stm32l4");
        assert_eq!(a.take(Bus::Usart, 1, Dir::Tx), None);
    }

    /// F7 is F2 plus room, never less - the one relationship between two of
    /// these tables that IS a pattern, and worth pinning so a future edit that
    /// narrows F7 shows up as a failure rather than a silent regression.
    #[test]
    fn f7_is_a_superset_of_f2() {
        for (bus, n, dir) in [
            (Bus::Usart, 1u8, Dir::Tx),
            (Bus::Usart, 3, Dir::Rx),
            (Bus::Spi, 2, Dir::Tx),
            (Bus::I2c, 2, Dir::Tx),
            (Bus::I2c, 3, Dir::Rx),
        ] {
            let f2 = candidates("stm32f2", bus, n, dir).unwrap_or(&[]);
            let f7 = candidates("stm32f7", bus, n, dir).unwrap_or(&[]);
            assert!(
                f2.iter().all(|c| f7.contains(c)),
                "F7 lost a channel F2 has: {bus:?}{n} {dir:?} - F2 {f2:?}, F7 {f7:?}"
            );
        }
    }

    /// F2 and F4 look interchangeable and are not. Sharing one table would have
    /// handed an F2 project channels its hardware does not route.
    #[test]
    fn f2_is_not_f4() {
        let pick = |fam: &str, bus, n, dir| DmaAllocator::new(fam).take(bus, n, dir);
        // USART2 RX: F4 falls back to DMA1_CH7, F2 has no second choice.
        let mut f4 = DmaAllocator::new("stm32f4");
        let mut f2 = DmaAllocator::new("stm32f2");
        f4.take(Bus::Usart, 2, Dir::Rx);
        f2.take(Bus::Usart, 2, Dir::Rx);
        assert!(
            f4.take(Bus::Usart, 2, Dir::Rx).is_some(),
            "F4 falls back to DMA1_CH7"
        );
        assert!(
            f2.take(Bus::Usart, 2, Dir::Rx).is_none(),
            "F2 has no second channel"
        );
        // USART3 exists on F2 and not on the F411 the F4 table came from.
        assert!(pick("stm32f2", Bus::Usart, 3, Dir::Rx).is_some());
        assert!(pick("stm32f4", Bus::Usart, 3, Dir::Rx).is_none());
        // I2C3 TX: two candidates on F4, one on F2.
        let mut f4 = DmaAllocator::new("stm32f4");
        let mut f2 = DmaAllocator::new("stm32f2");
        f4.take(Bus::I2c, 3, Dir::Tx);
        f2.take(Bus::I2c, 3, Dir::Tx);
        assert!(f4.take(Bus::I2c, 3, Dir::Tx).is_some(), "F4 has a spare");
        assert!(f2.take(Bus::I2c, 3, Dir::Tx).is_none(), "F2 does not");
    }

    #[test]
    fn the_channel_carries_both_of_its_names() {
        let mut a = DmaAllocator::new("stm32f4");
        let tx = a.take(Bus::Usart, 1, Dir::Tx).expect("USART1 TX");
        assert_eq!(tx.peri, "DMA2_CH7");
        // The name that goes in `bind_interrupts!` is NOT the same string.
        assert_eq!(tx.irq, "DMA2_STREAM7");
    }

    /// The F1 has one DMA1 channel per request and names the vector in full:
    /// `DMA1_CH4` is bound as `DMA1_CHANNEL4`. Both compiled on an F103C8.
    #[test]
    fn the_f1_takes_its_one_fixed_channel_per_request() {
        let mut a = DmaAllocator::new("stm32f1");
        let tx = a.take(Bus::Usart, 1, Dir::Tx).expect("USART1 TX");
        assert_eq!(
            (tx.peri.as_str(), tx.irq.as_str()),
            ("DMA1_CH4", "DMA1_CHANNEL4")
        );
        let rx = a.take(Bus::Usart, 1, Dir::Rx).expect("USART1 RX");
        assert_eq!(rx.peri, "DMA1_CH5");
        // No second choice: a channel already given is not given again.
        assert_eq!(a.take(Bus::Usart, 1, Dir::Tx), None);
        // SPI1 and I2C1 do not collide with USART1, and nothing is on DMA2.
        for (bus, n, dir) in [
            (Bus::Spi, 1u8, Dir::Tx),
            (Bus::Spi, 1, Dir::Rx),
            (Bus::I2c, 1, Dir::Tx),
            (Bus::I2c, 1, Dir::Rx),
        ] {
            let c = a
                .take(bus, n, dir)
                .unwrap_or_else(|| panic!("{bus:?}{n} {dir:?}"));
            assert!(c.peri.starts_with("DMA1_CH"), "{c:?}");
            assert_eq!(c.irq, c.peri.replace("_CH", "_CHANNEL"));
        }
    }

    #[test]
    fn an_allocated_channel_is_never_handed_out_twice() {
        // The overlap that makes an allocator necessary: SPI1_TX's first choice
        // is DMA2_CH3, USART1_RX's is DMA2_CH5, and both also list DMA2_CH2.
        let mut a = DmaAllocator::new("stm32f4");
        let spi_tx = a.take(Bus::Spi, 1, Dir::Tx).unwrap();
        let spi_rx = a.take(Bus::Spi, 1, Dir::Rx).unwrap();
        let u_rx = a.take(Bus::Usart, 1, Dir::Rx).unwrap();
        let all = [&spi_tx.peri, &spi_rx.peri, &u_rx.peri];
        let uniq: BTreeSet<_> = all.iter().collect();
        assert_eq!(uniq.len(), all.len(), "channels collided: {all:?}");
    }

    #[test]
    fn running_out_of_candidates_falls_back_rather_than_reusing() {
        let mut a = DmaAllocator::new("stm32f4");
        // SPI2 TX has exactly one candidate; taking it twice must fail the
        // second time instead of silently sharing the channel.
        assert!(a.take(Bus::Spi, 2, Dir::Tx).is_some());
        assert_eq!(a.take(Bus::Spi, 2, Dir::Tx), None);
    }

    #[test]
    fn an_unknown_instance_is_not_invented() {
        let mut a = DmaAllocator::new("stm32f4");
        assert_eq!(a.take(Bus::Usart, 3, Dir::Tx), None, "F411 has no USART3");
    }
}
