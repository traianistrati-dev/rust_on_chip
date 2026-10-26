//! STM32F1 on embassy: the AFIO remap generic on bus pins and the timer remap.

/// The pin traits of the USART, SPI, I2C and I2S config files, whose bounds
/// carry the F1's AFIO remap as a second parameter (`pin_trait!(.., @A)` in
/// embassy-stm32 0.6). `CkPin` is also the SD host's clock trait, which does
/// NOT take one - see [`with_afio_remap`].
pub(super) const AFIO_BUS_PIN_TRAITS: [&str; 14] = [
    "RxPin", "TxPin", "CtsPin", "RtsPin", "CkPin", "DePin", "SckPin", "MosiPin", "MisoPin",
    "SclPin", "SdaPin", "I2sSdPin", "WsPin", "MckPin",
];

/// A bus config file as an STM32F1 needs it on embassy-stm32: every pin trait
/// takes the AFIO remap as one more parameter (`RxPin<USART1, A>`), and embassy
/// writes AFIO_MAPR itself from it.
///
/// `init` gets a generic `A` that the pins decide at the call site: each bus
/// pad belongs to one remap set, so rustc infers it, and a pair from two sets
/// does not type-check (E0277) - the rule the F1's remap groups keep in the UI
/// anyway. Compiled on an STM32F103C8 with default and remapped pads, main.rs
/// unchanged. A pass over the text rather than a flag through every template,
/// so no other family's file can change; a file with no such bound is
/// returned as it came.
pub(super) fn with_afio_remap(body: &str) -> String {
    let mut out = body.to_owned();
    let mut bounds = 0;
    for tr in AFIO_BUS_PIN_TRAITS {
        let needle = format!("impl {tr}<peripherals::");
        let mut s = String::with_capacity(out.len() + 16);
        let mut rest = out.as_str();
        while let Some(at) = rest.find(&needle) {
            let start = at + needle.len();
            // The SD host's `CkPin` has no remap parameter: leave it as it is.
            if tr == "CkPin"
                && (rest[start..].starts_with("SDIO") || rest[start..].starts_with("SDMMC"))
            {
                s.push_str(&rest[..start]);
                rest = &rest[start..];
                continue;
            }
            let Some(close) = rest[start..].find('>') else {
                break;
            };
            s.push_str(&rest[..start + close]);
            s.push_str(", A");
            rest = &rest[start + close..];
            bounds += 1;
        }
        s.push_str(rest);
        out = s;
    }
    if bounds == 0 {
        return body.to_owned();
    }
    // `<'d, ` before `<'d>(`, or the first would match the second's result.
    out.replace("pub fn init<'d, ", "pub fn init<'d, A, ")
        .replace("pub fn init<'d>(", "pub fn init<'d, A>(")
}

/// The AFIO remap values USART{n}'s pad `pad` is listed under on an STM32F1 -
/// the `USARTn_REMAP` field values of RM0008 9.3.8, as stm32-metapac's `afio`
/// lists carry them for every F1 part - or `None` for a pad of a USART that
/// does not remap (UART4/5) or that this table has no row for.
///
/// Every USART pad belongs to its own remap set but four: USART1's CTS/RTS/CK
/// (PA11/PA12/PA8) and USART3's CTS/RTS (PB13/PB14) stay put in two sets. So
/// a TX or RX pad decides the set, and embassy's remap parameter `A` is
/// inferred from it.
pub(super) fn f1_usart_remaps(n: u8, pad: &str) -> Option<&'static [u8]> {
    Some(match (n, pad) {
        (1, "PA9" | "PA10") => &[0],
        (1, "PB6" | "PB7") => &[1],
        (1, "PA8" | "PA11" | "PA12") => &[0, 1],
        (2, "PA0" | "PA1" | "PA2" | "PA3" | "PA4") => &[0],
        (2, "PD3" | "PD4" | "PD5" | "PD6" | "PD7") => &[1],
        (3, "PB10" | "PB11" | "PB12") => &[0],
        (3, "PC10" | "PC11" | "PC12") => &[1],
        (3, "PB13" | "PB14") => &[0, 1],
        (3, "PD8" | "PD9" | "PD10" | "PD11" | "PD12") => &[3],
        _ => return None,
    })
}

/// The pads of `pads` (GPIO names) from two different remap sets of
/// USART{n}, as `(pad, pad)`, or `None` when one set holds them all. A USART's
/// pads move together on the F1, so a pair from two sets has no `A` that both
/// implement (E0277) - and wiring a CTS pad by hand on a 100-pin part can pick
/// the other set's.
pub(super) fn f1_usart_remap_clash<'p>(n: u8, pads: &[&'p str]) -> Option<(&'p str, &'p str)> {
    let mut common: Vec<u8> = vec![0, 1, 2, 3];
    let mut seen: Vec<(&'p str, &'static [u8])> = Vec::new();
    for &pad in pads {
        let Some(sets) = f1_usart_remaps(n, pad) else {
            continue;
        };
        common.retain(|v| sets.contains(v));
        if common.is_empty() {
            // Name the earlier pad that shares no set with this one.
            let other = seen
                .iter()
                .find(|(_, s)| !s.iter().any(|v| sets.contains(v)))
                .or(seen.first())
                .map_or(pad, |(p, _)| *p);
            return Some((other, pad));
        }
        seen.push((pad, sets));
    }
    None
}

/// embassy's AFIO remap type for one of stm32f1xx-hal's timer remap
/// type-states - the rows [`super::super::stm32::pwm_remap`] picks from. The numbers
/// are the TIMx_REMAP field values: TIM1 has no partial row here because its
/// CH1..4 pads are the same in both, TIM4's field is a single bit.
pub(super) fn embassy_afio_remap(hal_ty: &str) -> Option<&'static str> {
    Some(match hal_ty {
        "Tim1NoRemap" | "Tim2NoRemap" | "Tim3NoRemap" => "AfioRemap<0>",
        "Tim2PartialRemap1" => "AfioRemap<1>",
        "Tim2PartialRemap2" | "Tim3PartialRemap" => "AfioRemap<2>",
        "Tim1FullRemap" | "Tim2FullRemap" | "Tim3FullRemap" => "AfioRemap<3>",
        "Tim4NoRemap" => "AfioRemapBool<false>",
        "Tim4Remap" => "AfioRemapBool<true>",
        _ => return None,
    })
}

/// An F1 PWM file with its remap NAMED: a `Remap` alias in the generated block,
/// re-spliced when the wiring moves, and every `TimerPin` bound taking it.
pub(super) fn with_timer_remap(body: &str, n: u8, remap: &str) -> String {
    let alias = format!(
        "// The AFIO remap these pads are on - named, because a timer pad can sit in two.\n\
         pub type Remap = embassy_stm32::gpio::{remap};\n"
    );
    let end = "// <<< GENERATED END >>>";
    let body = match body.find(end) {
        Some(at) => format!("{}{alias}{}", &body[..at], &body[at..]),
        None => body.to_owned(),
    };
    let mut out = body;
    for ch in 1..=4u8 {
        out = out.replace(
            &format!("impl TimerPin<peripherals::TIM{n}, Ch{ch}>>"),
            &format!("impl TimerPin<peripherals::TIM{n}, Ch{ch}, Remap>>"),
        );
    }
    out
}
