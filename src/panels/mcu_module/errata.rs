//! Silicon errata the IDE can see coming from the pins alone.
//!
//! A pad note says what a pad IS; these say what a COMBINATION of choices
//! does, so they are worked out from the current selection. The same text goes
//! to the pin panel of every pad involved, to the interrupt picker, and into
//! the generated code above the lines it is about.
//!
//! Only conditions the vendor documents and the generated code really meets
//! are reported: each check names its erratum and why this project hits it.

use super::mcu::model::Mcu;
use super::pins::PinFunction;
use super::pins::logic::pin::Pin;

/// One erratum this selection runs into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clash {
    /// The pads involved, by GPIO name (`GPIO4`, `PA9`), in pin order.
    pub pads: Vec<String>,
    /// What goes wrong and what to change, one paragraph.
    pub text: String,
}

/// Every clash in `mcu`'s current selection.
pub fn clashes(mcu: &Mcu) -> Vec<Clash> {
    let pins: Vec<&Pin> = mcu.iter_all_pins().filter(|p| !p.reserved).collect();
    let mut out = esp32_irq_groups(&mcu.family, &pins);
    out.extend(f1_tim1_usart1(
        &mcu.name,
        &mcu.family,
        mcu.is_async(),
        &pins,
    ));
    out
}

/// The clash texts that involve pad `pin`, for its panel.
pub fn warnings_for(mcu: &Mcu, pin: &Pin) -> Vec<String> {
    clashes(mcu)
        .into_iter()
        .filter(|c| c.pads.iter().any(|g| g == pin.gpio() || *g == pin.name))
        .map(|c| c.text)
        .collect()
}

/// ESP32 erratum [GPIO-3.14] (ESP32 Series SoC Errata v3.0, section 3.11;
/// every revision, no fix scheduled): the GPIOs form two interrupt groups,
/// GPIO0-31 and GPIO32-39, and once a pad in a group raises an EDGE interrupt
/// no other interrupt may be used in that group - edges can be missed while
/// the group's shared STATUS register is read and cleared.
///
/// Every interrupt the IDE generates on an ESP is an edge (`wait_for_*_edge`
/// on Async, `Event::*Edge` on Blocking), so two armed inputs in one group are
/// the erratum exactly. Only the classic ESP32 has it.
pub fn esp32_irq_groups(family: &str, pins: &[&Pin]) -> Vec<Clash> {
    if family != "esp32" {
        return Vec::new();
    }
    let mut groups: [Vec<String>; 2] = [Vec::new(), Vec::new()];
    for p in pins {
        if p.reserved || p.selected_function != PinFunction::GpioInput || p.irq.is_none() {
            continue;
        }
        let Some(n) = p
            .gpio()
            .strip_prefix("GPIO")
            .and_then(|n| n.parse::<u8>().ok())
        else {
            continue;
        };
        groups[usize::from(n >= 32)].push(p.gpio().to_owned());
    }
    groups
        .into_iter()
        .zip(["GPIO0-31", "GPIO32-39"])
        .filter(|(pads, _)| pads.len() > 1)
        .map(|(pads, group)| Clash {
            text: format!(
                "ESP32 erratum GPIO-3.14: {} each raise an interrupt in the {group} group. With an edge \
                 interrupt in a group no other interrupt may be used there - edges can be missed \
                 while the group's shared status register is read and cleared. Keep one IRQ input \
                 per group (GPIO0-31, GPIO32-39) and poll the others.",
                and_list(&pads)
            ),
            pads,
        })
        .collect()
}

/// STM32F10x medium-density erratum ES096 2.3.10 (Rev 15): with USART1 and
/// TIM1 clocked, PA9 as an alternate-function output and TIM1_CH2 in PWM mode
/// WITHOUT its output, USART1 sends wrong characters on PA9. ST's only
/// workaround, remapping TIM1_CH2, needs the full remap of a 100-pin package.
///
/// The generated code meets it on the Async runtime alone: embassy's
/// `SimplePwm` puts all four TIM1 channels in PWM mode whichever are wired,
/// while stm32f1xx-hal (Blocking, RTIC) sets it only on the wired ones. CH2
/// itself is PA9, so with USART1 TX there CH2 is never the wired one.
pub fn f1_tim1_usart1(name: &str, family: &str, is_async: bool, pins: &[&Pin]) -> Option<Clash> {
    if family != "stm32f1" || !is_async || !medium_density_f10x(name) {
        return None;
    }
    let tx = pins
        .iter()
        .find(|p| p.selected_function == PinFunction::UsartTx(1) && p.gpio() == "PA9")?;
    let pwm: Vec<&&Pin> = pins
        .iter()
        .filter(|p| matches!(p.selected_function, PinFunction::TimerPwm { timer: 1, .. }))
        .collect();
    if pwm.is_empty() {
        return None;
    }
    let mut pads: Vec<String> = pwm.iter().map(|p| p.gpio().to_owned()).collect();
    pads.push(tx.gpio().to_owned());
    Some(Clash {
        text: format!(
            "Erratum ES096 2.3.10: TIM1 PWM on {} is built by embassy's SimplePwm (Async), which \
             also puts TIM1 CH2 in PWM mode with no output - and with USART1 TX on PA9 the UART \
             then sends wrong characters. Change one side: USART1 TX on PB6 (remap), the PWM on \
             another timer, or the Blocking runtime, whose stm32f1xx-hal leaves unwired channels \
             alone.",
            and_list(&pads[..pads.len() - 1])
        ),
        pads,
    })
}

/// STM32F101/102/103 with 64 or 128 KB of flash (`x8`/`xB`): the parts ES096
/// covers. The others have their own errata sheets.
fn medium_density_f10x(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    let Some(rest) = n.strip_prefix("STM32F10") else {
        return false;
    };
    let b = rest.as_bytes();
    b.len() >= 3 && matches!(b[0], b'1'..=b'3') && matches!(b[2], b'8' | b'B')
}

/// `A`, `A and B`, `A, B and C`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// `text` as `//` comment lines at `indent`, wrapped near 80 columns, for the
/// generated code.
pub fn comment(text: &str, indent: &str) -> String {
    let width = 80usize.saturating_sub(indent.len() + 3).max(20);
    let mut out = String::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            out.push_str(&format!("{indent}// {line}\n"));
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push_str(&format!("{indent}// {line}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::builtins;
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::pins::logic::pin::Edge;

    fn chip(id: &str) -> Mcu {
        builtins::builtin_for(id).unwrap().build_mcu()
    }

    fn set(mcu: &mut Mcu, name: &str, f: PinFunction, irq: Option<Edge>) {
        let p = mcu
            .iter_all_pins_mut()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("no pad {name}"));
        p.selected_function = f;
        p.irq = irq;
    }

    #[test]
    fn two_armed_inputs_in_one_esp32_group_clash() {
        let mut mcu = chip("esp32");
        set(
            &mut mcu,
            "GPIO4",
            PinFunction::GpioInput,
            Some(Edge::Rising),
        );
        set(&mut mcu, "GPIO5", PinFunction::GpioInput, Some(Edge::Both));
        set(
            &mut mcu,
            "GPIO34",
            PinFunction::GpioInput,
            Some(Edge::Falling),
        );
        let c = clashes(&mcu);
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!(c[0].pads, ["GPIO4", "GPIO5"]);
        assert!(
            c[0].text.contains("GPIO4 and GPIO5 each raise"),
            "{}",
            c[0].text
        );
        assert!(c[0].text.contains("GPIO0-31 group"), "{}", c[0].text);
        // Both pads see it; the lone pad in the other group does not.
        let pad = |n: &str| mcu.iter_all_pins().find(|p| p.name == n).unwrap().clone();
        assert_eq!(warnings_for(&mcu, &pad("GPIO5")).len(), 1);
        assert!(warnings_for(&mcu, &pad("GPIO34")).is_empty());
    }

    #[test]
    fn one_irq_per_group_or_a_polled_input_is_fine() {
        let mut mcu = chip("esp32");
        set(
            &mut mcu,
            "GPIO4",
            PinFunction::GpioInput,
            Some(Edge::Rising),
        );
        set(
            &mut mcu,
            "GPIO34",
            PinFunction::GpioInput,
            Some(Edge::Rising),
        );
        set(&mut mcu, "GPIO5", PinFunction::GpioInput, None);
        assert!(clashes(&mcu).is_empty());
        // The erratum is the classic ESP32's alone.
        let mut c3 = chip("esp32c3");
        set(&mut c3, "GPIO4", PinFunction::GpioInput, Some(Edge::Rising));
        set(&mut c3, "GPIO5", PinFunction::GpioInput, Some(Edge::Rising));
        assert!(clashes(&c3).is_empty());
    }

    fn f103(runtime: Runtime) -> Mcu {
        let mut mcu = chip("stm32f103c8t6");
        mcu.runtime = runtime;
        set(
            &mut mcu,
            "PA8",
            PinFunction::TimerPwm {
                timer: 1,
                channel: 1,
            },
            None,
        );
        set(&mut mcu, "PA9", PinFunction::UsartTx(1), None);
        mcu
    }

    #[test]
    fn tim1_pwm_beside_usart1_tx_on_pa9_clashes_on_async_only() {
        let mcu = f103(Runtime::Async);
        let c = clashes(&mcu);
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!(c[0].pads, ["PA8", "PA9"]);
        assert!(c[0].text.contains("ES096 2.3.10"), "{}", c[0].text);
        // stm32f1xx-hal leaves the unwired CH2 alone.
        assert!(clashes(&f103(Runtime::Blocking)).is_empty());
    }

    #[test]
    fn moving_either_side_clears_the_f103_clash() {
        let mut tx_on_pb6 = f103(Runtime::Async);
        set(&mut tx_on_pb6, "PA9", PinFunction::Unset, None);
        set(&mut tx_on_pb6, "PB6", PinFunction::UsartTx(1), None);
        assert!(clashes(&tx_on_pb6).is_empty());
        let mut pwm_on_tim2 = f103(Runtime::Async);
        set(&mut pwm_on_tim2, "PA8", PinFunction::Unset, None);
        set(
            &mut pwm_on_tim2,
            "PA0",
            PinFunction::TimerPwm {
                timer: 2,
                channel: 1,
            },
            None,
        );
        assert!(clashes(&pwm_on_tim2).is_empty());
    }

    #[test]
    fn only_the_parts_es096_covers_are_checked() {
        for (name, yes) in [
            ("STM32F103C8Tx", true),
            ("STM32F103RBTx", true),
            ("STM32F101CBTx", true),
            ("STM32F103RCTx", false),
            ("STM32F103ZETx", false),
            ("STM32F100RBTx", false),
            ("STM32F407VGTx", false),
        ] {
            assert_eq!(medium_density_f10x(name), yes, "{name}");
        }
    }

    /// The generated code says it where the clash is, on both ESP runtimes.
    #[test]
    fn the_esp32_main_names_the_erratum_above_its_interrupts() {
        for runtime in [Runtime::Blocking, Runtime::Async] {
            let mut mcu = chip("esp32");
            mcu.runtime = runtime;
            set(
                &mut mcu,
                "GPIO4",
                PinFunction::GpioInput,
                Some(Edge::Rising),
            );
            set(
                &mut mcu,
                "GPIO5",
                PinFunction::GpioInput,
                Some(Edge::Falling),
            );
            let main = mcu.fresh_main_rs();
            let irq = main
                .find("// ── GPIO interrupts ──")
                .expect("an IRQ section");
            let note = main
                .find("// ESP32 erratum GPIO-3.14:")
                .unwrap_or_else(|| panic!("{main}"));
            assert!(note > irq, "{main}");
            // One IRQ per group: no comment.
            set(&mut mcu, "GPIO5", PinFunction::GpioInput, None);
            assert!(!mcu.fresh_main_rs().contains("GPIO-3.14"));
        }
    }

    #[test]
    fn the_f103_async_main_names_the_erratum_above_the_tim1_pwm() {
        let main = f103(Runtime::Async).fresh_main_rs();
        let note = main
            .find("// Erratum ES096 2.3.10:")
            .unwrap_or_else(|| panic!("{main}"));
        let pwm = main
            .find("pins::configs::pwm1::init(")
            .unwrap_or_else(|| panic!("{main}"));
        assert!(note < pwm, "{main}");
        // Only comment lines between it and the PWM init.
        let from = main[..note].rfind('\n').map_or(0, |i| i + 1);
        let to = main[..pwm].rfind('\n').unwrap();
        assert!(
            main[from..to].lines().all(|l| l.starts_with("    // ")),
            "{main}"
        );
        assert!(!f103(Runtime::Blocking).fresh_main_rs().contains("ES096"));
    }

    #[test]
    fn a_comment_wraps_and_keeps_every_word() {
        let text = "one two three four five six seven eight nine ten eleven twelve thirteen \
                    fourteen fifteen sixteen seventeen eighteen nineteen twenty";
        let c = comment(text, "    ");
        assert!(
            c.lines().all(|l| l.starts_with("    // ") && l.len() <= 80),
            "{c}"
        );
        let words: Vec<&str> = c.lines().flat_map(|l| l[7..].split(' ')).collect();
        assert_eq!(
            words.join(" "),
            text.split_whitespace().collect::<Vec<_>>().join(" ")
        );
    }
}
