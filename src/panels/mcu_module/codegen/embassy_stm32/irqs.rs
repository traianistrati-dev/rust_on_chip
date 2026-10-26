//! The `bind_interrupts!` block: one key per vector, every handler it carries.

use super::*;

/// The head of the `bind_interrupts!` block.
pub(super) const IRQS_HEAD: &str = include_str!("../templates/embassy_stm32/irqs_head.rs.tmpl");

/// The same head while some `init` still waits for its DMA channels.
pub(super) const IRQS_HEAD_DMA_TODO: &str =
    include_str!("../templates/embassy_stm32/irqs_head_dma_todo.rs.tmpl");

/// The head of the Blocking runtime's block, which binds the comparators alone.
pub(super) const IRQS_HEAD_BLOCKING: &str =
    include_str!("../templates/embassy_stm32/irqs_head_blocking.rs.tmpl");

/// The struct the Blocking runtime binds the comparators in. Not `Irqs`:
/// before the comparators, this runtime bound nothing, so a Blocking project's
/// own code may hold an `Irqs` of its own (for an RNG, a USB) - and a `use` of
/// `bind_interrupts` or `peripherals`, which is why the block names neither.
pub(crate) const BLOCKING_COMP_IRQS: &str = "CompIrqs";

/// The Blocking runtime's `bind_interrupts!`: `(vector, instance)` per
/// comparator, grouped one key per vector as [`dma_irqs_block`] groups them -
/// a G4's `COMP1_2_3` or a U5's `COMP` carries several - with every path
/// spelled out from `embassy_stm32`.
pub(super) fn blocking_comp_irqs_block(comp_binds: &[(String, u8)]) -> String {
    let mut by_irq: Vec<(&str, Vec<u8>)> = Vec::new();
    for (vector, n) in comp_binds {
        match by_irq.iter_mut().find(|(v, _)| *v == vector.as_str()) {
            Some((_, ns)) if !ns.contains(n) => ns.push(*n),
            Some(_) => {}
            None => by_irq.push((vector.as_str(), vec![*n])),
        }
    }
    let mut b = tmpl::render(IRQS_HEAD_BLOCKING, &[("{STRUCT}", BLOCKING_COMP_IRQS)]);
    for (vector, ns) in by_irq {
        let handlers: Vec<String> = ns
            .iter()
            .map(|n| {
                format!(
                    "embassy_stm32::comp::InterruptHandler<embassy_stm32::peripherals::COMP{n}>"
                )
            })
            .collect();
        b.push_str(&format!("    {vector} => {};\n", handlers.join(", ")));
    }
    b.push_str("});\n\n");
    b
}

/// The `bind_interrupts!` block `main.rs` needs when any bus runs on DMA.
///
/// It has to live here rather than in each `configs/*.rs`, because embassy takes
/// ONE value that must satisfy every binding at once — the peripheral's own
/// interrupts and both DMA channels' — and the channels are chosen at this call
/// site. `i2c_instances` are the I2C peripherals whose event/error interrupts
/// belong in the same struct.
pub(super) fn dma_irqs_block(
    i2c_instances: &[u8],
    // `(peripheral word, instance, buffered)` — "USART"/"LPUART", its number,
    // and which of the two interrupt handlers its `init` takes.
    usart_instances: &[(&str, u8, bool)],
    binds: &[(String, String)],
    irqs: &[String],
    comp_binds: &[(String, u8)],
    // `(vector, handler type)` bound VERBATIM — for a peripheral whose handler
    // is neither a DMA channel's nor one of the shapes above. The SD-card
    // controller is the first: it brings its own interrupt into the same
    // `Irqs`, because its `init` takes one value for that and the channel both.
    extra_binds: &[(String, String)],
    // Whether some `init` below was left with the `DMA_TODO` placeholder: the
    // header asks for channels exactly then - not merely because none were
    // bound, which is also the state of a project with nothing on DMA at all
    // (a buffered USART, an armed input).
    unresolved_dma: bool,
) -> String {
    let head = if unresolved_dma {
        IRQS_HEAD_DMA_TODO
    } else {
        IRQS_HEAD
    };

    // One entry per INTERRUPT, listing every handler it carries. Grouping is
    // not a nicety: `bind_interrupts!` rejects a repeated key, and repeats are
    // normal — an STM32G0's `DMA1_Channel2_3` covers two channels, its `I2C1`
    // carries both I2C halves, its `USART3_4_LPUART1` two USARTs.
    let mut by_irq: Vec<(String, Vec<String>)> = Vec::new();
    let mut bind = |irq: String, handler: String| match by_irq.iter_mut().find(|(k, _)| *k == irq) {
        Some((_, hs)) => {
            if !hs.contains(&handler) {
                hs.push(handler);
            }
        }
        None => by_irq.push((irq, vec![handler])),
    };

    for n in i2c_instances {
        let ev = format!("embassy_stm32::i2c::EventInterruptHandler<peripherals::I2C{n}>");
        let er = format!("embassy_stm32::i2c::ErrorInterruptHandler<peripherals::I2C{n}>");
        // Two vectors on most families, ONE on G0/C0/L0/U0 - where the split
        // names do not exist and the macro cannot compile them.
        match nvic::i2c_irqs(irqs, *n) {
            Some(nvic::I2cIrqs::Combined(v)) => {
                bind(v.clone(), ev);
                bind(v, er);
            }
            Some(nvic::I2cIrqs::Split { ev: e, er: r }) => {
                bind(e, ev);
                bind(r, er);
            }
            None => {
                bind(format!("I2C{n}_EV"), ev);
                bind(format!("I2C{n}_ER"), er);
            }
        }
    }
    for (irq, peri) in binds {
        bind(
            irq.clone(),
            format!("embassy_stm32::dma::InterruptHandler<peripherals::{peri}>"),
        );
    }
    for (irq, handler) in extra_binds {
        bind(irq.clone(), handler.clone());
    }
    for (vector, n) in comp_binds {
        // Several comparators share one vector on the G4, so this goes through
        // the same grouping as everything else.
        bind(
            vector.clone(),
            format!("embassy_stm32::comp::InterruptHandler<peripherals::COMP{n}>"),
        );
    }
    for (peri, n, buffered) in usart_instances {
        // EVERY async serial binds here, buffered or DMA — a vector may be bound
        // only once in the program, and on an STM32G0 USART3, USART4 and LPUART1
        // share `USART3_4_LPUART1`, so the grouping above is the only thing that
        // lets two of them coexist. The handler type is the one its `init` asks
        // for: `BufferedInterruptHandler` for the ring-buffer driver,
        // `InterruptHandler` for the DMA one.
        let handler = if *buffered {
            "BufferedInterruptHandler"
        } else {
            "InterruptHandler"
        };
        bind(
            serial_irq(irqs, peri, *n),
            format!("embassy_stm32::usart::{handler}<peripherals::{peri}{n}>"),
        );
    }

    let mut b = String::from(head);
    for (irq, handlers) in &by_irq {
        b.push_str(&format!(
            "    {irq} => {};
",
            handlers.join(", ")
        ));
    }
    b.push_str(
        "});

",
    );
    b
}

#[cfg(test)]
mod irq_key_tests {
    use super::*;

    fn v(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    /// STM32G0: one I2C vector, one shared USART vector, and DMA channels that
    /// share vectors too - every key in the struct has to be unique, because
    /// `bind_interrupts!` rejects a repeat.
    #[test]
    fn a_chip_that_shares_vectors_gets_one_line_per_vector() {
        let irqs = v(&["I2C1", "USART1", "USART3_4_LPUART1", "DMA1_Channel2_3"]);
        let binds = [
            ("DMA1_CHANNEL2_3".to_owned(), "DMA1_CH2".to_owned()),
            ("DMA1_CHANNEL2_3".to_owned(), "DMA1_CH3".to_owned()),
        ];
        let out = dma_irqs_block(&[1], &[("USART", 3, false)], &binds, &irqs, &[], &[], false);
        assert!(
            out.contains(
                "    I2C1 => embassy_stm32::i2c::EventInterruptHandler<peripherals::I2C1>, embassy_stm32::i2c::ErrorInterruptHandler<peripherals::I2C1>;"
            ),
            "{out}"
        );
        assert!(
            !out.contains("I2C1_EV"),
            "the split names do not exist here:
{out}"
        );
        assert!(
            out.contains("    USART3_4_LPUART1 => embassy_stm32::usart::InterruptHandler<peripherals::USART3>;"),
            "{out}"
        );
        assert!(
            out.contains("DMA1_CHANNEL2_3 => embassy_stm32::dma::InterruptHandler<peripherals::DMA1_CH2>, embassy_stm32::dma::InterruptHandler<peripherals::DMA1_CH3>;"),
            "{out}"
        );
        // No key twice, whatever the source.
        let keys: Vec<&str> = out
            .lines()
            .filter_map(|l| l.split_once(" => ").map(|(k, _)| k.trim()))
            .collect();
        let mut uniq = keys.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(
            keys.len(),
            uniq.len(),
            "duplicate key in:
{out}"
        );
    }

    /// The overwhelming majority of families, and every chip imported before the
    /// vector list existed: the split names stay.
    #[test]
    fn a_chip_with_split_vectors_or_no_list_keeps_ev_and_er() {
        for irqs in [v(&["I2C1_EV", "I2C1_ER", "USART1"]), Vec::new()] {
            let out = dma_irqs_block(&[1], &[("USART", 1, false)], &[], &irqs, &[], &[], false);
            assert!(
                out.contains(
                    "    I2C1_EV => embassy_stm32::i2c::EventInterruptHandler<peripherals::I2C1>;"
                ),
                "{out}"
            );
            assert!(
                out.contains(
                    "    I2C1_ER => embassy_stm32::i2c::ErrorInterruptHandler<peripherals::I2C1>;"
                ),
                "{out}"
            );
            assert!(
                out.contains(
                    "    USART1 => embassy_stm32::usart::InterruptHandler<peripherals::USART1>;"
                ),
                "{out}"
            );
        }
    }

    /// The buffered-USART config file has its OWN `bind_interrupts!`, keyed the
    /// same way.
    #[test]
    fn the_buffered_usart_binds_the_chips_vector_in_main() {
        // The config file no longer binds anything: a vector can be bound ONCE
        // per program, and on this chip USART3 shares one with USART4/LPUART1.
        let f = serial_config_file("USART", 3, None, "USART3_4_LPUART1", true);
        assert!(!f.contains("bind_interrupts!(struct Irqs"), "{f}");
        assert!(f.contains("irqs: impl Binding<"), "{f}");
        // main.rs binds it, under the chip's own vector name.
        let out = dma_irqs_block(
            &[],
            &[("USART", 3, true)],
            &[],
            &["USART3_4_LPUART1".into()],
            &[],
            &[],
            false,
        );
        assert!(
            out.contains(
                "    USART3_4_LPUART1 => embassy_stm32::usart::BufferedInterruptHandler<peripherals::USART3>;"
            ),
            "{out}"
        );
    }
}
