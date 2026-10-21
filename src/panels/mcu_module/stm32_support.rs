//! Which STM32 parts a project can be built for at all, and why not.
//!
//! Two kinds of part reach the importer and can never build:
//!
//! * the STM32MP1 / MP2 **Cortex-A microprocessors**. They boot Linux from
//!   external memory; their Cortex-M, where they have one, is a coprocessor the
//!   Cortex-A loads. RustOnChip writes bare-metal Cortex-M firmware, and
//!   embassy-stm32 has no STM32MP chip at all;
//! * parts **embassy-stm32 publishes no chip feature for** - two whole families
//!   (WL3, WB0), lines newer than its chip list, the modules. Their Cargo.toml
//!   names a feature no resolver can satisfy, which also takes rust-analyzer
//!   down for the whole project.
//!
//! Both used to import anyway, with an amber "imported with gaps" line, and sit
//! in the chip list with New Project enabled. Now the import refuses them with
//! the reason, the chip search lists them greyed with it, and a definition that
//! got in earlier is blocked in New Project rather than dropped: one that
//! vanished from the list would be its own mystery.
//!
//! Judged offline, against the feature list harvested from the crate (see
//! [`super::stm32_embassy_features`]), so it answers per search row with no
//! network and covers parts no list of known gaps would name.

use super::mcu_def::McuDefinition;
use super::stm32_pin_data::{
    EMBASSY_CRATE, EMBASSY_VERSION, embassy_chip_feature, embassy_feature_in, embassy_publishes,
};

/// Why a part cannot be built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// An STM32MP1 / MP2. `no_m_core` on the MP13, which has no Cortex-M at all.
    CortexA { no_m_core: bool },
    /// embassy-stm32 publishes no `feature` for this part; `note` says why,
    /// when the line is one whose reason is known.
    NoHal {
        feature: String,
        note: Option<&'static str>,
    },
}

impl Refusal {
    /// The few words a search row has room for.
    pub fn tag(&self) -> &'static str {
        match self {
            Refusal::CortexA { .. } => "Cortex-A MPU",
            Refusal::NoHal { .. } => "no HAL",
        }
    }

    /// One sentence: what the part is, and why no project for it builds.
    pub fn reason(&self) -> String {
        match self {
            Refusal::CortexA { no_m_core: true } => concat!(
                "an STM32MP13 is a Cortex-A7 microprocessor with no Cortex-M core: ",
                "RustOnChip builds bare-metal firmware for Cortex-M microcontrollers, ",
                "and embassy-stm32 has no STM32MP chip"
            )
            .to_owned(),
            Refusal::CortexA { no_m_core: false } => concat!(
                "an STM32MP1/MP2 is a Cortex-A microprocessor that boots Linux from ",
                "external memory, its Cortex-M a coprocessor the Cortex-A loads: ",
                "RustOnChip builds bare-metal firmware for Cortex-M microcontrollers, ",
                "and embassy-stm32 has no STM32MP chip"
            )
            .to_owned(),
            Refusal::NoHal { feature, note } => format!(
                concat!(
                    "{krate} {version} publishes no `{feature}` chip feature{note}, ",
                    "so no project for it can resolve its dependencies"
                ),
                krate = EMBASSY_CRATE,
                version = EMBASSY_VERSION,
                feature = feature,
                note = note.map(|n| format!(" - {n}")).unwrap_or_default(),
            ),
        }
    }
}

/// Why a line has no feature, by feature prefix. The first match wins; a part
/// on none of them gets the plain sentence.
const LINE_NOTES: &[(&str, &str)] = &[
    (
        "stm32wl3",
        "the STM32WL3 sub-GHz parts are in neither embassy-stm32 nor stm32-metapac",
    ),
    (
        "stm32wl4",
        "the STM32WL4 sub-GHz parts are in neither embassy-stm32 nor stm32-metapac",
    ),
    (
        "stm32wb0",
        "stm32-metapac lists the STM32WB0, but embassy-stm32 builds no chip for it",
    ),
    ("stm32wb1m", "a module has no chip feature of its own"),
    ("stm32wb5m", "a module has no chip feature of its own"),
    ("stm32wl5m", "a module has no chip feature of its own"),
    ("stm32wba5m", "a module has no chip feature of its own"),
    ("stm32wba6m", "a module has no chip feature of its own"),
    ("stm32g411", "the line is newer than its chip list"),
    ("stm32g414", "the line is newer than its chip list"),
    ("stm32h5e", "the line is newer than its chip list"),
    ("stm32h5f", "the line is newer than its chip list"),
    ("stm32h543", "the line is newer than its chip list"),
    ("stm32h553", "the line is newer than its chip list"),
    ("stm32u335", "the line is newer than its chip list"),
    ("stm32u345", "the line is newer than its chip list"),
    ("stm32u356", "the line is newer than its chip list"),
    ("stm32u366", "the line is newer than its chip list"),
    ("stm32u3b5", "the line is newer than its chip list"),
    ("stm32u3c5", "the line is newer than its chip list"),
    ("stm32wba2", "the line is newer than its chip list"),
];

fn note_for(feature: &str) -> Option<&'static str> {
    LINE_NOTES
        .iter()
        .find(|(prefix, _)| feature.starts_with(prefix))
        .map(|(_, note)| *note)
}

fn slug(part: &str) -> String {
    part.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// An STM32MP, by family key, part number or core list.
///
/// The part number counts on its own: open-pin-data has no family index, so
/// its rows carry a family GUESSED from the name, and a definition imported
/// before this lists the M4 or M33 as its CPU.
fn cortex_a(family: &str, part: &str, cores: &[String]) -> Option<Refusal> {
    let lower: Vec<String> = cores.iter().map(|c| c.to_ascii_lowercase()).collect();
    let mp =
        family.to_ascii_lowercase().starts_with("stm32mp") || slug(part).starts_with("stm32mp");
    if !mp && !lower.iter().any(|c| c.contains("cortex-a")) {
        return None;
    }
    let no_m_core = slug(part).starts_with("stm32mp13")
        || (!lower.is_empty() && !lower.iter().any(|c| c.contains("cortex-m")));
    Some(Refusal::CortexA { no_m_core })
}

/// Whether a VENDOR part can be built: `family` is the IDE family key, `part`
/// the vendor part number, `cores` the vendor's `<Core>` list (empty when the
/// source has none).
///
/// The F1 is judged by nothing here: it builds on `stm32f1xx-hal` off Async,
/// and on Async from its part number's first eleven characters, which every
/// F1 part has a feature for.
pub fn refusal(family: &str, part: &str, cores: &[String]) -> Option<Refusal> {
    if let Some(r) = cortex_a(family, part, cores) {
        return Some(r);
    }
    let family = family.to_ascii_lowercase();
    if !family.starts_with("stm32") || family == "stm32f1" {
        return None;
    }
    let feature = embassy_chip_feature(part);
    (!embassy_publishes(&feature)).then(|| Refusal::NoHal {
        note: note_for(&feature),
        feature,
    })
}

/// Whether a DEFINITION can be built: judged on the dependency line its
/// project really carries, so one whose name is not a part number - a board
/// the user authored - is judged by the feature they chose.
///
/// A line pinning another embassy-stm32 version is not judged: the list here
/// is this version's, and the network check in the System tab answers for the
/// other.
pub fn refusal_for_def(def: &McuDefinition) -> Option<Refusal> {
    refusal_for(
        &def.family,
        &def.display_name,
        &def.cpu,
        &def.project.hal_dep,
    )
}

/// [`refusal_for_def`] from the four fields it reads, for the MCU form, which
/// holds them before there is a definition.
pub fn refusal_for(family: &str, name: &str, cpu: &str, hal_dep: &str) -> Option<Refusal> {
    // A definition's ONE cpu field is its application core, not the part's
    // core list: an MP157 typed as "ARM Cortex-A7" still has its M4. Only the
    // part number says there is no Cortex-M at all.
    if cortex_a(family, name, &[cpu.to_owned()]).is_some() {
        return Some(Refusal::CortexA {
            no_m_core: slug(name).starts_with("stm32mp13"),
        });
    }
    let pinned = format!("version = \"{EMBASSY_VERSION}\"");
    let feature = embassy_feature_in(hal_dep).filter(|_| hal_dep.contains(&pinned))?;
    (!embassy_publishes(feature)).then(|| Refusal::NoHal {
        note: note_for(feature),
        feature: feature.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cores(list: &[&str]) -> Vec<String> {
        list.iter().map(|c| c.to_string()).collect()
    }

    /// Every kind of part that cannot build, and the near neighbours of each
    /// that can - a refusal that caught one of those would be worse than none.
    #[test]
    fn refuses_exactly_what_cannot_be_built() {
        let a7m4 = cores(&["ARM Cortex-A7", "ARM Cortex-M4"]);
        assert_eq!(
            refusal("stm32mp1", "STM32MP157CAAx", &a7m4),
            Some(Refusal::CortexA { no_m_core: false })
        );
        assert_eq!(
            refusal("stm32mp1", "STM32MP131AAEx", &cores(&["ARM Cortex-A7"])),
            Some(Refusal::CortexA { no_m_core: true })
        );
        assert_eq!(
            refusal(
                "stm32mp2",
                "STM32MP257CAIx",
                &cores(&["ARM Cortex-A35", "ARM Cortex-M33"])
            ),
            Some(Refusal::CortexA { no_m_core: false })
        );
        // open-pin-data: no cores, a family guessed from the name.
        assert!(matches!(
            refusal("stm32mp", "STM32MP151AAAx", &[]),
            Some(Refusal::CortexA { .. })
        ));

        for (family, part, note) in [
            ("stm32wl3", "STM32WL30KBVx", Some("STM32WL3")),
            ("stm32wl", "STM32WL33KCVxX", Some("STM32WL3")),
            ("stm32wb0", "STM32WB09TEFx", Some("WB0")),
            ("stm32wl4", "STM32WL40CCVx", Some("STM32WL4")),
            ("stm32g4", "STM32G411CBTx", Some("newer")),
            ("stm32h5", "STM32H5E4AJIx", Some("newer")),
            ("stm32h5", "STM32H563LIHxQ", None),
            ("stm32u3", "STM32U3B5CGTx", Some("newer")),
            ("stm32wba", "STM32WBA25CEUx", Some("newer")),
            ("stm32wb", "STM32WB5MMGHx", Some("module")),
            ("stm32wl", "STM32WL5MOCHxS", Some("module")),
            ("stm32f7", "STM32F723RCVx", None),
            ("stm32l4", "STM32L485JCYx", None),
        ] {
            let r = refusal(family, part, &[]).unwrap_or_else(|| panic!("{part} must be refused"));
            let Refusal::NoHal { feature, .. } = &r else {
                panic!("{part}: {r:?}");
            };
            let why = r.reason();
            assert!(why.contains(feature.as_str()), "{why}");
            if let Some(n) = note {
                assert!(why.contains(n), "{part}: {why}");
            }
            assert_eq!(r.tag(), "no HAL");
        }

        for (family, part, c) in [
            ("stm32h7", "STM32H743ZITx", vec![]),
            (
                "stm32h7",
                "STM32H745ZITx",
                cores(&["ARM Cortex-M7", "ARM Cortex-M4"]),
            ),
            (
                "stm32wl",
                "STM32WL55JCIx",
                cores(&["ARM Cortex-M4", "ARM Cortex-M0+"]),
            ),
            ("stm32wl", "STM32WLE5JCIx", vec![]),
            ("stm32l1", "STM32L151RETx", vec![]),
            ("stm32l1", "STM32L151VDTxX", vec![]),
            ("stm32l1", "STM32L151C6TxA", vec![]),
            ("stm32u5", "STM32U575ZITxQ", vec![]),
            ("stm32n6", "STM32N645A0HxQ", vec![]),
            ("stm32n6", "STM32N645A0HxQG", vec![]),
            ("stm32c0", "STM32C071C8TxN", vec![]),
            ("stm32f1", "STM32F103C8Tx", vec![]),
            ("stm32wba", "STM32WBA55CGUx", vec![]),
            ("stm32wb", "STM32WB35CEUxA", vec![]),
            ("stm32g4", "STM32G431CBTx", vec![]),
            ("esp32c3", "esp32c3", vec![]),
            ("rp2040", "rp2040", vec![]),
            ("nrf52833", "nrf52833", vec![]),
        ] {
            assert_eq!(refusal(family, part, &c), None, "{part}");
        }
    }

    /// A definition is judged on its own line: a board the user named freely
    /// builds if its feature exists, a legacy WB0 import does not, and a line
    /// pinning another version is not this list's to judge.
    #[test]
    fn a_definition_is_judged_on_the_line_it_builds_with() {
        let base = crate::panels::mcu_module::builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.family == "stm32f1")
            .expect("the bundled STM32F1");
        let with = |family: &str, name: &str, line: &str| {
            let mut d = base.clone();
            d.family = family.into();
            d.display_name = name.into();
            d.project.hal_dep = line.into();
            d
        };
        let line =
            |f: &str| format!("embassy-stm32 = {{ version = \"0.6\", features = [\"{f}\"] }}");
        assert_eq!(
            refusal_for_def(&base),
            None,
            "stm32f1xx-hal: nothing to judge"
        );
        assert_eq!(
            refusal_for_def(&with("stm32f4", "My board", &line("stm32f411re"))),
            None
        );
        let wb0 = refusal_for_def(&with("stm32wb0", "STM32WB09TEFx", &line("stm32wb09te")));
        assert!(
            matches!(&wb0, Some(Refusal::NoHal { feature, .. }) if feature == "stm32wb09te"),
            "{wb0:?}"
        );
        let other = "embassy-stm32 = { version = \"0.7\", features = [\"stm32wl30kb\"] }";
        assert_eq!(
            refusal_for_def(&with("stm32wl3", "STM32WL30KBVx", other)),
            None
        );
        // An MP imported before this, built for its M4: still the MPU.
        let mut mp = with("stm32mp1", "STM32MP157CAAx", &line("stm32mp157ca"));
        mp.cpu = "ARM Cortex-M4".into();
        assert_eq!(
            refusal_for_def(&mp),
            Some(Refusal::CortexA { no_m_core: false })
        );
        // A form with the MPU's Cortex-A typed as its CPU, or none yet: the
        // MP157 still has an M4 - only the MP13 has none.
        for cpu in ["ARM Cortex-A7", ""] {
            assert_eq!(
                refusal_for("stm32mp1", "STM32MP157CAAx", cpu, ""),
                Some(Refusal::CortexA { no_m_core: false }),
                "{cpu:?}"
            );
        }
        assert_eq!(
            refusal_for("", "STM32MP131AAEx", "ARM Cortex-A7", ""),
            Some(Refusal::CortexA { no_m_core: true })
        );
    }

    /// Every chip the IDE ships with builds - including the F103, whose name
    /// is judged by its stm32f1xx-hal line, i.e. not at all.
    #[test]
    fn every_bundled_chip_is_buildable() {
        for d in crate::panels::mcu_module::builtins::builtin_definitions() {
            assert_eq!(refusal_for_def(&d), None, "{}", d.id);
        }
    }

    /// Against the vendor database: refused if and only if the part has a
    /// Cortex-A core or its derived feature is not published. Ignored because
    /// it needs STM32CubeMX.
    ///
    /// `cargo test -- --ignored the_refusal_matches_the_vendor_database --nocapture`
    #[test]
    #[ignore]
    fn the_refusal_matches_the_vendor_database() {
        let Some(db) = crate::panels::mcu_module::clock::graph::cubemx::default_db_dir() else {
            println!("skipped: no STM32CubeMX database on this machine");
            return;
        };
        let mut files: Vec<_> = std::fs::read_dir(db.join("mcu"))
            .expect("the CubeMX mcu folder")
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|e| e == "xml")
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("STM32"))
            })
            .collect();
        files.sort();
        let (mut parts, mut cortex, mut nohal) = (0usize, 0usize, 0usize);
        for path in &files {
            let xml = std::fs::read_to_string(path).expect("readable");
            for chip in
                crate::panels::mcu_module::stm32_pin_data::convert_xml(&xml).expect("converts")
            {
                parts += 1;
                let f = &chip.form;
                match refusal(&f.family, &f.display_name, &chip.cores) {
                    Some(Refusal::CortexA { .. }) => cortex += 1,
                    Some(Refusal::NoHal { .. }) => nohal += 1,
                    None => {}
                }
            }
        }
        println!("{parts} parts: {cortex} Cortex-A, {nohal} without a HAL feature");
        assert_eq!(cortex, 232, "MP1 84 + MP2 148");
        assert_eq!(nohal, 170, "WL3 22 + WB0 10 + 138 single lines");
    }
}
