//! Deterministic chip-identity helpers — derive the boring fields (family, CPU,
//! toolchain, Rust target) from an STM32 part NAME, so authoring a new chip can
//! start by just typing its name. Pure + tested; reused by the New MCU form's
//! "Auto-fill from name" button and (later) the AI datasheet import.

use super::mcu_catalog::ToolchainKind;

/// The STM32 family key (as used by the codegen / clock backends) from a part
/// name. `STM32WBA55CG` → `stm32wba`, `STM32F411RE` → `stm32f4`, `STM32F103C8`
/// → `stm32f1`, `STM32G0B1RE` → `stm32g0`, `STM32H743` → `stm32h7`. `None` for
/// a non-STM32 / unrecognised name.
pub fn family_from_name(name: &str) -> Option<String> {
    let n = name.trim().to_ascii_lowercase();
    let rest = n.strip_prefix("stm32")?;
    // Multi-letter families carry no series digit in the key (WBA/WB/WL).
    for p in ["wba", "wb", "wl"] {
        if rest.starts_with(p) {
            return Some(format!("stm32{p}"));
        }
    }
    // Single-letter family + series digit (F1, F4, G0, H7, L4, U5, C0, …).
    let mut it = rest.chars();
    let letter = it.next().filter(|c| c.is_ascii_alphabetic())?;
    let digit = it.next().filter(|c| c.is_ascii_digit())?;
    Some(format!("stm32{letter}{digit}"))
}

/// The Cortex core label for a family key (`stm32wba` → `Cortex-M33`). `None`
/// for families not mapped yet.
pub fn cpu_for_family(family: &str) -> Option<&'static str> {
    Some(match family {
        "stm32f0" | "stm32g0" | "stm32c0" | "stm32l0" | "stm32u0" => "Cortex-M0+",
        "stm32f1" | "stm32f2" | "stm32l1" => "Cortex-M3",
        "stm32f3" | "stm32f4" | "stm32g4" | "stm32l4" | "stm32wb" | "stm32wl" => "Cortex-M4",
        "stm32f7" | "stm32h7" => "Cortex-M7",
        "stm32l5" | "stm32u5" | "stm32h5" | "stm32wba" => "Cortex-M33",
        _ => return None,
    })
}

/// The Rust target triple for a Cortex core label on a part of `family`.
/// Delegates to the converter's [`target_for`](super::stm32_pin_data::target_for)
/// so the form and the XML importer agree on the mapping - including the
/// STM32WL, whose M4 has no FPU.
pub fn target_for_cpu(family: &str, cpu: &str) -> &'static str {
    super::stm32_pin_data::target_for(family, cpu)
}

/// The whole identity a part name implies: `(family, cpu, toolchain, target)`.
/// `None` when the name isn't a recognised STM32 part.
pub fn identity_from_name(
    name: &str,
) -> Option<(String, &'static str, ToolchainKind, &'static str)> {
    let family = family_from_name(name)?;
    let cpu = cpu_for_family(&family)?;
    let target = target_for_cpu(&family, cpu);
    Some((family, cpu, ToolchainKind::RustEmbedded, target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_from_name_covers_single_and_multi_letter() {
        assert_eq!(
            family_from_name("STM32WBA55CG").as_deref(),
            Some("stm32wba")
        );
        assert_eq!(family_from_name("STM32F411RE").as_deref(), Some("stm32f4"));
        assert_eq!(
            family_from_name("STM32F103C8T6").as_deref(),
            Some("stm32f1")
        );
        assert_eq!(family_from_name("STM32G0B1RE").as_deref(), Some("stm32g0"));
        assert_eq!(family_from_name("STM32H743ZI").as_deref(), Some("stm32h7"));
        assert_eq!(family_from_name("stm32wb55").as_deref(), Some("stm32wb"));
        assert_eq!(family_from_name("STM32WLE5").as_deref(), Some("stm32wl"));
        assert_eq!(family_from_name("STM32U585").as_deref(), Some("stm32u5"));
        // Not an STM32 part.
        assert_eq!(family_from_name("ESP32-C3"), None);
        assert_eq!(family_from_name(""), None);
    }

    #[test]
    fn identity_maps_name_to_the_full_tuple() {
        let (fam, cpu, tc, target) = identity_from_name("STM32WBA55CG").unwrap();
        assert_eq!(fam, "stm32wba");
        assert_eq!(cpu, "Cortex-M33");
        assert_eq!(tc, ToolchainKind::RustEmbedded);
        assert_eq!(target, "thumbv8m.main-none-eabihf");

        let (fam, cpu, _, target) = identity_from_name("STM32F411RE").unwrap();
        assert_eq!(
            (fam.as_str(), cpu, target),
            ("stm32f4", "Cortex-M4", "thumbv7em-none-eabihf")
        );

        let (fam, cpu, _, target) = identity_from_name("STM32F103C8").unwrap();
        assert_eq!(
            (fam.as_str(), cpu, target),
            ("stm32f1", "Cortex-M3", "thumbv7m-none-eabi")
        );

        let (_, cpu, _, target) = identity_from_name("STM32G0B1RE").unwrap();
        assert_eq!((cpu, target), ("Cortex-M0+", "thumbv6m-none-eabi"));

        // Dual-core parts are named for their application core: the H7's M7
        // (hard-float) and the WL's M4, which has no FPU on any STM32WL.
        let (_, cpu, _, target) = identity_from_name("STM32H745ZI").unwrap();
        assert_eq!((cpu, target), ("Cortex-M7", "thumbv7em-none-eabihf"));
        let (_, cpu, _, target) = identity_from_name("STM32WL55JC").unwrap();
        assert_eq!((cpu, target), ("Cortex-M4", "thumbv7em-none-eabi"));
        let (_, _, _, target) = identity_from_name("STM32WLE5JC").unwrap();
        assert_eq!(target, "thumbv7em-none-eabi");

        assert!(identity_from_name("RP2040").is_none());
    }
}
