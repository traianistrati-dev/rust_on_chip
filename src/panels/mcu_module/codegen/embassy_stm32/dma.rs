//! DMA channel arguments for the `init`s: from the chip's map, the module, or the TODO.

use super::*;

/// Whether a SPI/I2C module runs in async-DMA mode (else blocking).
pub(super) fn is_dma(mode: AsyncBusMode) -> bool {
    mode == AsyncBusMode::AsyncDma
}

/// The DMA-channel placeholders for an async-DMA init call. Deliberately
/// undefined fields (`p.DMA_TX_TODO`) so the project fails to compile at exactly
/// this line until the user supplies channels valid for the peripheral on their
/// chip — the IDE can't know them (it doesn't model DMA).
///
/// `Irqs` is the LAST argument: embassy 0.6 makes the caller bind the DMA
/// channels' interrupts, and only this file knows which channels those are (see
/// `dma_irqs_block`). A one-way UART's `init` takes ONE channel, so it gets
/// that half of the placeholder alone (`dma_todo`).
pub(super) const DMA_TODO: &str = "p.DMA_TX_TODO, p.DMA_RX_TODO, Irqs";

/// [`DMA_TODO`] for the halves `wants` names: the arguments the `init` has, so
/// filling in the placeholders gives a call that compiles.
pub(super) fn dma_todo(wants: (bool, bool)) -> String {
    match wants {
        (true, false) => "p.DMA_TX_TODO, Irqs".to_owned(),
        (false, true) => "p.DMA_RX_TODO, Irqs".to_owned(),
        _ => DMA_TODO.to_owned(),
    }
}

/// Whether some `init` in `calls` was left with a channel placeholder - either
/// half of one.
pub(super) fn has_dma_todo(calls: &str) -> bool {
    calls.contains("DMA_TX_TODO") || calls.contains("DMA_RX_TODO")
}

/// The DMA arguments for one peripheral: real channels when the family has a
/// table (see [`dma_map`]), the `TODO` placeholder otherwise.
///
/// Pushes the matching `bind_interrupts!` lines into `binds`, because the two
/// have to agree and deriving them apart is how they drift.
pub(super) fn dma_args(
    alloc: &mut dma_map::DmaAllocator,
    binds: &mut Vec<(String, String)>,
    uses: &mut Vec<dma_map::DmaUse>,
    bus: dma_map::Bus,
    n: u8,
    label: &str,
    manual: (&str, &str),
    // Which halves this peripheral really uses. A TX-only UART must not take an
    // RX channel out of circulation for a transfer that never happens.
    wants: (bool, bool),
) -> (String, String) {
    // A channel the user pinned in the Virtual Module wins over allocation; it
    // was already reserved, so `take_named` only has to resolve its interrupt.
    // `clash` records the one refusal the user can act on without looking
    // anything up - the same channel pinned to two Virtual Modules - so the
    // TODO below can say that instead of a generic "pass valid channels".
    let mut clash: Option<String> = None;
    let mut pick = |alloc: &mut dma_map::DmaAllocator, named: &str, dir| {
        if named.is_empty() {
            return alloc.take(bus, n, dir);
        }
        match alloc.take_named_or(named) {
            Ok(p) => Some(p),
            Err(dma_map::NamedRefusal::AlreadyTaken) => {
                clash = Some(named.to_owned());
                None
            }
            Err(dma_map::NamedRefusal::UnknownIrq) => None,
        }
    };
    // Both or neither: a TX reserved next to a failed RX would take a channel
    // out of circulation for a peripheral that still ends up on the TODO path.
    let tx = wants
        .0
        .then(|| pick(alloc, manual.0, dma_map::Dir::Tx))
        .unwrap_or(Some(dma_map::DmaPick::default()));
    let rx = wants
        .1
        .then(|| pick(alloc, manual.1, dma_map::Dir::Rx))
        .unwrap_or(Some(dma_map::DmaPick::default()));
    let (Some(tx), Some(rx)) = (tx, rx) else {
        // Say exactly what is missing, at the line that needs it. Two
        // different sentences, because they ask for two different actions.
        //
        // Built line by line rather than as one continued literal: a backslash
        // continuation carries the source indentation into the emitted file the
        // moment rustfmt reflows it, and this text lands in the user's main.rs.
        let mut note = String::new();
        if let Some(ch) = &clash {
            note.push_str(&format!(
                "    // TODO(async DMA): `{ch}` is already taken by another peripheral.\n"
            ));
            note.push_str("    //   Two Virtual Modules are pinned to it - give one of them a\n");
            note.push_str("    //   different channel, or set it back to Automatic.\n");
        } else {
            note.push_str(&format!(
                "    // TODO(async DMA): pass DMA channels valid for {label} on this chip,\n"
            ));
            note.push_str("    //   and bind their interrupts in the `Irqs` block above.\n");
        }
        return (dma_todo(wants), note);
    };
    // Kept as a PAIR, not a finished line: two channels can share one
    // interrupt (STM32G0's `DMA1_Channel2_3`), and `bind_interrupts!` wants
    // those as two handlers on one key, not the key twice. Only
    // `dma_irqs_block` sees them all, so only it can group them.
    for c in [&tx, &rx].into_iter().filter(|c| !c.peri.is_empty()) {
        binds.push((c.irq.clone(), c.peri.clone()));
    }
    for (c, dir, pinned) in [
        (&tx, dma_map::Dir::Tx, manual.0),
        (&rx, dma_map::Dir::Rx, manual.1),
    ]
    .into_iter()
    .filter(|(c, _, _)| !c.peri.is_empty())
    {
        uses.push(dma_map::DmaUse {
            peri: c.peri.clone(),
            irq: c.irq.clone(),
            user: format!("{label} {}", dir.label()),
            manual: !pinned.is_empty(),
        });
    }
    // Resolved — no note. A TODO telling the user to do work already done is
    // worse than none: it makes correct output look unfinished.
    let chans: Vec<String> = [&tx, &rx]
        .into_iter()
        .filter(|c| !c.peri.is_empty())
        .map(|c| format!("p.{}", c.peri))
        .collect();
    (format!("{}, Irqs", chans.join(", ")), String::new())
}

/// The channels a module pinned by hand, as `(tx, rx)`; `("", "")` when it
/// pinned none, which is the normal case.
pub(super) fn manual_channels<'a, C>(
    cfg: Option<&'a C>,
    get: impl Fn(&'a C) -> (&'a str, &'a str),
) -> (&'a str, &'a str) {
    cfg.map(get).unwrap_or(("", ""))
}
