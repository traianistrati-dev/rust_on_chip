//! The IoT tab's "Which chips carry what": every chip the IDE knows, and which
//! links and protocols the generator writes on each.
//!
//! Read off the functions the generator decides with - [`iot::availability`]
//! for the links, [`iot::tls_available`] / [`iot::https_available`] for TLS -
//! so the table cannot promise what the code does not do. A test builds every
//! bundled chip and checks each cell against [`iot::active`].

use crate::panels::mcu_module::iot::{self, Availability, Link, Platform};
use crate::panels::mcu_module::mcu_def::McuDefinition;
use eframe::egui;

use super::iot_diagrams::{BLOCKED, GENERATED};

const DIM: egui::Color32 = egui::Color32::from_rgb(130, 130, 145);

const NEEDS_WIFI: &str = "needs the Wi-Fi station, which this chip has not";
const USB_WIRED: &str = "not while USB is wired: both need the CLOCK_POWER vector";
const THREAD_BESIDE_WIFI: &str =
    "alone on the radio: not beside Wi-Fi, ESP-NOW or Bluetooth (the other link's switch wins)";
const THREAD_BESIDE_BLE: &str = "alone on the radio: not beside Bluetooth (its switch wins)";
const MESH_BESIDE_BLE: &str =
    "one Bluetooth host: not beside the Nordic UART link (its switch wins)";
const MESH_NEEDS_STORE: &str =
    "needs the flash store (Configuration tab): it keeps the node's sequence number there";
const HTTPS_TIGHT: &str =
    "beside MQTT over TLS about 15 KB of stack is left: it links, but was never run on a board";
const SUB_GHZ: &str = "its radio is sub-GHz (LoRa, FSK), which carries none of these links";

/// A column of the table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Column {
    Wifi,
    EspNow,
    Ble,
    Thread,
    BleMesh,
    Mqtt,
    MqttTls,
    Http,
    Https,
    Sntp,
}

impl Column {
    pub(super) const ALL: [Column; 10] = [
        Column::Wifi,
        Column::EspNow,
        Column::Ble,
        Column::Thread,
        Column::BleMesh,
        Column::Mqtt,
        Column::MqttTls,
        Column::Http,
        Column::Https,
        Column::Sntp,
    ];

    fn label(self) -> &'static str {
        match self {
            Column::Wifi => "Wi-Fi",
            Column::EspNow => "ESP-NOW",
            Column::Ble => "Bluetooth LE",
            Column::Thread => "Thread",
            Column::BleMesh => "Bluetooth Mesh",
            Column::Mqtt => "MQTT",
            Column::MqttTls => "MQTT + TLS",
            Column::Http => "HTTP",
            Column::Https => "HTTPS",
            Column::Sntp => "SNTP",
        }
    }

    /// The link this column is, `None` for a protocol over the station.
    fn link(self) -> Option<Link> {
        match self {
            Column::Wifi => Some(Link::Wifi),
            Column::EspNow => Some(Link::EspNow),
            Column::Ble => Some(Link::Ble),
            Column::Thread => Some(Link::Thread),
            Column::BleMesh => Some(Link::BleMesh),
            _ => None,
        }
    }
}

/// One cell: what the generator writes for a column on a chip.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Cell {
    /// Generated - in every combination when there are no notes, else with
    /// the limits they name.
    Yes(Vec<&'static str>),
    /// The chip can carry it, and a later phase generates it: why not yet.
    Later(&'static str),
    /// Not on this chip: why.
    No(&'static str),
}

/// What `column` gets on `family` (`cyw43` = the board carries that radio).
pub(super) fn cell(column: Column, family: &str, cyw43: bool) -> Cell {
    let wifi = iot::platform(family, cyw43).is_some();
    // Bluetooth beside Wi-Fi is a combination only where there is Bluetooth.
    let ble = iot::ble_platform(family, cyw43);
    let notes = |n: [Option<&'static str>; 3]| Cell::Yes(n.into_iter().flatten().collect());
    if let Some(link) = column.link() {
        return match iot::availability(link, family, cyw43) {
            Availability::Ready => match column {
                Column::Ble => notes([
                    (ble == Some(Platform::Nrf)).then_some(USB_WIRED),
                    None,
                    None,
                ]),
                Column::Thread if wifi => notes([Some(THREAD_BESIDE_WIFI), None, None]),
                Column::Thread => notes([Some(THREAD_BESIDE_BLE), None, None]),
                Column::BleMesh => notes([Some(MESH_BESIDE_BLE), Some(MESH_NEEDS_STORE), None]),
                _ => Cell::Yes(Vec::new()),
            },
            Availability::Planned(why) => Cell::Later(why),
            Availability::NotHere(why) => Cell::No(why),
        };
    }
    if !wifi {
        return Cell::No(NEEDS_WIFI);
    }
    match column {
        Column::MqttTls => match iot::tls_available(family, false) {
            Err(why) => Cell::No(why),
            Ok(()) => notes([ble.and(iot::tls_available(family, true).err()), None, None]),
        },
        Column::Https => match iot::https_available(family, false, false) {
            Err(why) => Cell::No(why),
            Ok(()) => notes([
                ble.and(iot::https_available(family, true, false).err()),
                iot::https_available(family, false, true).err(),
                iot::https_tight(family, true).then_some(HTTPS_TIGHT),
            ]),
        },
        _ => Cell::Yes(Vec::new()),
    }
}

/// A chip something is generated on.
pub(super) struct Row {
    id: String,
    name: String,
    cells: Vec<Cell>,
}

/// A chip and what the table says of it outside the grid.
pub(super) struct Entry {
    id: String,
    name: String,
    /// A radio, nothing generated: why, link by link.
    why: Vec<String>,
}

/// The chips, three ways.
#[derive(Default)]
pub(super) struct Table {
    /// Something is generated on these: the grid.
    generated: Vec<Row>,
    /// A radio, and nothing generated on it (yet).
    waiting: Vec<Entry>,
    /// No radio at all.
    no_radio: Vec<Entry>,
    /// Links generated on none of the chips, and why - asked of a chip that
    /// can carry the link, when there is one.
    never: Vec<(Link, String)>,
}

/// The board carries the CYW43 radio: its pad, the fact [`iot::has_cyw43`]
/// reads off a built chip, read here off the definition.
fn carries_cyw43(def: &McuDefinition) -> bool {
    let p = &def.pins;
    p.top
        .iter()
        .chain(&p.bottom)
        .chain(&p.left)
        .chain(&p.right)
        .chain(p.grid.iter().flat_map(|g| g.cells.iter().map(|c| &c.pin)))
        .any(|pin| pin.name == iot::CYW43_PAD)
}

/// Why `link` is not generated on `family`, as a line: `None` when it is.
fn why_not(link: Link, family: &str, cyw43: bool) -> Option<String> {
    match iot::availability(link, family, cyw43) {
        Availability::Ready => None,
        Availability::Planned(r) => Some(format!("{}, later: {r}", link.label())),
        Availability::NotHere(r) => Some(format!("{}: {r}", link.label())),
    }
}

/// The table for `defs`, in their order.
pub(super) fn table(defs: &[McuDefinition]) -> Table {
    let mut t = Table::default();
    let mut ready: Vec<Link> = Vec::new();
    let mut chips: Vec<(&str, bool)> = Vec::new();
    for def in defs {
        let family = def.family.as_str();
        let cyw43 = carries_cyw43(def);
        chips.push((family, cyw43));
        ready.extend(
            Link::ALL
                .into_iter()
                .filter(|l| iot::availability(*l, family, cyw43) == Availability::Ready),
        );
        let cells: Vec<Cell> = Column::ALL
            .iter()
            .map(|c| cell(*c, family, cyw43))
            .collect();
        let entry = |why| Entry {
            id: def.id.clone(),
            name: def.display_name.clone(),
            why,
        };
        let any_link = Column::ALL
            .iter()
            .zip(&cells)
            .any(|(c, cell)| c.link().is_some() && matches!(cell, Cell::Yes(_)));
        if any_link {
            t.generated.push(Row {
                id: def.id.clone(),
                name: def.display_name.clone(),
                cells,
            });
            continue;
        }
        let links = iot::links_for(family, cyw43);
        if links.is_empty() && !iot::has_subghz_radio(family) {
            t.no_radio.push(entry(Vec::new()));
            continue;
        }
        let mut why: Vec<String> = links
            .into_iter()
            .filter(|l| Column::ALL.iter().any(|c| c.link() == Some(*l)))
            .filter_map(|l| why_not(l, family, cyw43))
            .collect();
        if why.is_empty() {
            why.push(SUB_GHZ.to_owned());
        }
        t.waiting.push(entry(why));
    }
    t.never = Link::ALL
        .into_iter()
        .filter(|l| !ready.contains(l))
        .filter_map(|l| {
            let (family, cyw43) = chips
                .iter()
                .copied()
                .find(|(f, c)| iot::chip_can_carry(l, f, *c))
                .unwrap_or(("", false));
            Some((l, why_not(l, family, cyw43)?))
        })
        .collect();
    t
}

fn text(s: impl Into<String>) -> egui::RichText {
    egui::RichText::new(s.into()).size(11.5)
}

fn dim(s: impl Into<String>) -> egui::RichText {
    egui::RichText::new(s.into()).size(11.0).color(DIM)
}

/// "Which chips carry what", closed until opened.
pub(super) fn section(ui: &mut egui::Ui, defs: &[McuDefinition], current: &str) {
    egui::CollapsingHeader::new(egui::RichText::new("Which chips carry what").size(12.0))
        .id_salt("iot_chip_table")
        .default_open(false)
        .show(ui, |ui| body(ui, &table(defs), current));
}

fn chip_name(ui: &mut egui::Ui, name: &str, current: bool) {
    if current {
        ui.label(text(name).strong().color(GENERATED))
            .on_hover_text("This project's chip");
    } else {
        ui.label(text(name));
    }
}

fn cell_ui(ui: &mut egui::Ui, cell: &Cell) {
    match cell {
        Cell::Yes(notes) if notes.is_empty() => {
            ui.label(text("yes").color(GENERATED));
        }
        Cell::Yes(notes) => {
            let tip = notes
                .iter()
                .fold("Generated, with these limits:".to_owned(), |t, n| {
                    t + "\n- " + n
                });
            ui.label(text("yes*").color(GENERATED)).on_hover_text(tip);
        }
        Cell::Later(why) => {
            ui.label(text("later").color(BLOCKED))
                .on_hover_text(format!("Not generated yet: {why}"));
        }
        Cell::No(why) => {
            ui.label(text("no").color(DIM)).on_hover_text(*why);
        }
    }
}

fn body(ui: &mut egui::Ui, t: &Table, current: &str) {
    ui.label(dim(concat!(
        "What the IDE generates on each chip it knows, on the Async runtime (on Blocking, ",
        "nothing). Hover a cell for its limits or the reason."
    )));
    ui.add_space(4.0);
    egui::ScrollArea::horizontal()
        .id_salt("iot_chip_table_scroll")
        .show(ui, |ui| {
            egui::Grid::new("iot_chip_grid")
                .striped(true)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    ui.label(text("Chip").strong());
                    for c in Column::ALL {
                        ui.label(text(c.label()).strong());
                    }
                    ui.end_row();
                    for row in &t.generated {
                        chip_name(ui, &row.name, row.id == current);
                        for cell in &row.cells {
                            cell_ui(ui, cell);
                        }
                        ui.end_row();
                    }
                });
        });
    ui.add_space(6.0);
    ui.label(dim(concat!(
        "AWS IoT - publishing and the Device Shadow - runs over TLS with the board's ",
        "certificate: where MQTT + TLS or HTTPS says yes. Restart on panic comes on every ",
        "chip above, once a link is generated."
    )));
    if !t.never.is_empty() {
        ui.label(dim("Generated on no chip:"));
        ui.indent("iot_chip_never", |ui| {
            for (_, line) in &t.never {
                ui.label(dim(line.as_str()));
            }
        });
    }
    if !t.waiting.is_empty() {
        ui.add_space(6.0);
        ui.label(text("A radio, nothing generated yet").strong());
        for e in &t.waiting {
            chip_name(ui, &e.name, e.id == current);
            ui.indent(("iot_chip_why", &e.id), |ui| {
                for line in &e.why {
                    ui.label(dim(line.as_str()));
                }
            });
        }
    }
    if !t.no_radio.is_empty() {
        ui.add_space(6.0);
        let names: Vec<String> = t
            .no_radio
            .iter()
            .map(|e| {
                if e.id == current {
                    format!("{} (this project)", e.name)
                } else {
                    e.name.clone()
                }
            })
            .collect();
        ui.label(dim(format!(
            "No radio, nothing to generate: {}.",
            names.join(", ")
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::builtin_definitions;
    use crate::panels::mcu_module::flash_store::{self, FlashStoreConfig};
    use crate::panels::mcu_module::iot::{
        Active, BleConfig, BleMeshConfig, EspNowConfig, HttpConfig, IotConfig, MqttConfig,
        SntpConfig, ThreadConfig,
    };
    use crate::panels::mcu_module::mcu::model::Runtime;

    fn at(cells: &[Cell], c: Column) -> &Cell {
        &cells[Column::ALL.iter().position(|x| *x == c).unwrap()]
    }

    fn yes(cells: &[Cell], c: Column) -> bool {
        matches!(at(cells, c), Cell::Yes(_))
    }

    fn notes(cells: &[Cell], c: Column) -> usize {
        match at(cells, c) {
            Cell::Yes(n) => n.len(),
            _ => 0,
        }
    }

    /// Every cell of every bundled chip is what the generator does with that
    /// switch (and its neighbours) on: `iot::active`, the ONE decision.
    #[test]
    fn every_cell_is_what_the_generator_does() {
        for def in builtin_definitions() {
            let mut base = def.build_mcu();
            base.runtime = Runtime::Async;
            base.iot = IotConfig {
                restart_on_panic: false,
                ..IotConfig::default()
            };
            let on = |set: &dyn Fn(&mut IotConfig)| -> Option<Active> {
                let mut mcu = base.clone();
                set(&mut mcu.iot);
                iot::active(&mcu)
            };
            let ble = || Some(BleConfig::default());
            let mqtt = |tls| {
                Some(MqttConfig {
                    tls,
                    ..MqttConfig::default()
                })
            };
            let http = |tls| {
                Some(HttpConfig {
                    tls,
                    ..HttpConfig::default()
                })
            };
            let cells: Vec<Cell> = Column::ALL
                .iter()
                .map(|c| cell(*c, &def.family, carries_cyw43(&def)))
                .collect();
            let id = def.id.as_str();
            let has = |c: Column, a: Option<Active>, f: fn(&Active) -> bool| {
                assert_eq!(a.as_ref().is_some_and(f), yes(&cells, c), "{id}: {c:?}");
            };
            has(Column::Wifi, on(&|i| i.wifi = true), |a| a.station);
            has(
                Column::EspNow,
                on(&|i| i.esp_now = Some(EspNowConfig::default())),
                |a| a.esp_now,
            );
            has(Column::Ble, on(&|i| i.ble = ble()), |a| a.ble);
            has(
                Column::Thread,
                on(&|i| i.thread = Some(ThreadConfig::default())),
                |a| a.thread,
            );
            // Bluetooth Mesh keeps its sequence number in the flash store: on
            // with it, as the tab switches it - and the notes are the
            // combinations the generator drops.
            let mesh = |ble: bool, store: bool| {
                let mut mcu = base.clone();
                mcu.iot.ble_mesh = Some(BleMeshConfig::default());
                if ble {
                    mcu.iot.ble = Some(BleConfig::default());
                }
                if let (true, Ok(p)) = (store, flash_store::platform_of(&mcu)) {
                    mcu.flash_store = Some(FlashStoreConfig::default_on(&p, &mcu.family));
                }
                iot::active(&mcu)
            };
            has(Column::BleMesh, mesh(false, true), |a| a.ble_mesh);
            if yes(&cells, Column::BleMesh) {
                assert!(!mesh(true, true).is_some_and(|a| a.ble_mesh), "{id}: Mesh beside the Nordic UART link");
                assert!(!mesh(false, false).is_some_and(|a| a.ble_mesh), "{id}: Mesh without the flash store");
                assert_eq!(notes(&cells, Column::BleMesh), 2, "{id}: Bluetooth Mesh");
            }
            let wifi = |i: &mut IotConfig| i.wifi = true;
            has(
                Column::Mqtt,
                on(&|i| {
                    wifi(i);
                    i.mqtt = mqtt(false)
                }),
                |a| a.mqtt,
            );
            has(
                Column::MqttTls,
                on(&|i| {
                    wifi(i);
                    i.mqtt = mqtt(true)
                }),
                |a| a.mqtt_tls,
            );
            has(
                Column::Http,
                on(&|i| {
                    wifi(i);
                    i.http = http(false)
                }),
                |a| a.http,
            );
            has(
                Column::Https,
                on(&|i| {
                    wifi(i);
                    i.http = http(true)
                }),
                |a| a.http_tls,
            );
            has(
                Column::Sntp,
                on(&|i| {
                    wifi(i);
                    i.sntp = Some(SntpConfig::default())
                }),
                |a| a.sntp,
            );

            // A limit in a note is a combination the generator drops.
            let ble_radio = yes(&cells, Column::Ble);
            if yes(&cells, Column::Thread) {
                let beside_ble = on(&|i| {
                    i.thread = Some(ThreadConfig::default());
                    i.ble = ble()
                });
                assert!(
                    !beside_ble.is_some_and(|a| a.thread),
                    "{id}: Thread beside Bluetooth"
                );
                if yes(&cells, Column::Wifi) {
                    let beside = on(&|i| {
                        i.thread = Some(ThreadConfig::default());
                        wifi(i)
                    });
                    assert!(
                        !beside.is_some_and(|a| a.thread),
                        "{id}: Thread beside Wi-Fi"
                    );
                }
            }
            if yes(&cells, Column::MqttTls) {
                let beside = on(&|i| {
                    wifi(i);
                    i.mqtt = mqtt(true);
                    i.ble = ble()
                });
                let dropped = ble_radio && !beside.is_some_and(|a| a.mqtt_tls);
                assert_eq!(
                    notes(&cells, Column::MqttTls),
                    usize::from(dropped),
                    "{id}: MQTT + TLS"
                );
            }
            if yes(&cells, Column::Https) {
                let beside_ble = on(&|i| {
                    wifi(i);
                    i.http = http(true);
                    i.ble = ble()
                });
                let beside_mqtt = on(&|i| {
                    wifi(i);
                    i.http = http(true);
                    i.mqtt = mqtt(true)
                });
                let dropped = usize::from(ble_radio && !beside_ble.is_some_and(|a| a.http_tls))
                    + usize::from(!beside_mqtt.is_some_and(|a| a.http_tls))
                    + usize::from(iot::https_tight(&def.family, true));
                assert_eq!(notes(&cells, Column::Https), dropped, "{id}: HTTPS");
            }
        }
    }

    /// The definition's pad and the built chip's say the same.
    #[test]
    fn the_radio_pad_is_read_as_the_generator_reads_it() {
        for def in builtin_definitions() {
            assert_eq!(
                carries_cyw43(&def),
                iot::has_cyw43(&def.build_mcu()),
                "{}",
                def.id
            );
        }
    }

    /// A link generated on some chip has a column, so the table never leaves
    /// one out - and "generated on no chip" lists the rest.
    #[test]
    fn every_generated_link_has_a_column() {
        let t = table(&builtin_definitions());
        for link in Link::ALL {
            let column = Column::ALL.iter().any(|c| c.link() == Some(link));
            assert!(
                column || t.never.iter().any(|(l, _)| *l == link),
                "{link:?}"
            );
        }
        let never: Vec<Link> = t.never.iter().map(|(l, _)| *l).collect();
        assert_eq!(never, [Link::Zigbee, Link::EspWifiMesh]);
        // Asked of a chip that can carry it: the ESP's reason, not "only on an ESP".
        let esp = t.never.iter().find(|(l, _)| *l == Link::EspWifiMesh).map(|(_, why)| why.as_str());
        assert!(esp.is_some_and(|why| why.contains("ESP-IDF only")), "{esp:?}");
    }

    /// A cell's reason never names a radio the chip has not: availability's
    /// last arms were written for the links a chip can carry, and the grid
    /// asks it of every column.
    #[test]
    fn no_reason_claims_a_radio_the_chip_lacks() {
        let mut chips: Vec<(String, bool)> = builtin_definitions()
            .iter()
            .map(|d| (d.family.clone(), carries_cyw43(d)))
            .collect();
        for f in [
            "stm32f1", "stm32g4", "stm32wl", "rp2040", "esp32p4", "nrf52811",
        ] {
            chips.push((f.to_owned(), false));
        }
        for (family, cyw43) in &chips {
            for c in Column::ALL {
                let Some(link) = c.link() else { continue };
                if iot::chip_can_carry(link, family, *cyw43) {
                    continue;
                }
                let Cell::No(why) = cell(c, family, *cyw43) else {
                    panic!("{family}: {c:?} without the radio")
                };
                assert!(
                    !why.contains("has an 802.15.4 radio") && !why.contains("wpan"),
                    "{family}: {c:?}: {why}"
                );
            }
        }
    }

    /// The bundled chips, split as the generator stands today.
    #[test]
    fn the_bundled_chips_split_three_ways() {
        let t = table(&builtin_definitions());
        let ids = |v: Vec<&str>| v.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            t.generated.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
            ids(vec![
                "esp32",
                "esp32c2",
                "esp32c3",
                "esp32c5",
                "esp32c6",
                "esp32c61",
                "esp32h2",
                "esp32s2",
                "esp32s3",
                "rp2040_pico_w",
                "rp2350_pico2_w",
                "nrf52833_microbit_v2",
                "nrf52840_dk",
                "nrf52832_dk",
            ])
        );
        assert_eq!(
            t.waiting.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
            ids(vec!["nrf5340_dk", "nrf54l15_dk"])
        );
        assert!(t.waiting.iter().all(|e| !e.why.is_empty()));
        assert_eq!(
            t.no_radio.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
            ids(vec![
                "stm32f103c8t6",
                "rp2040_pico",
                "rp2350_pico2",
                "rp2350_pico2_ice"
            ])
        );
    }

    /// The section opened draws its grid, with the current chip marked.
    #[test]
    fn the_table_draws() {
        fn texts(s: &egui::Shape, out: &mut Vec<(String, egui::Color32)>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| texts(s, out)),
                egui::Shape::Text(t) => {
                    let color = t.galley.job.sections.first().map(|s| s.format.color);
                    out.push((t.galley.text().to_owned(), color.unwrap_or_default()));
                }
                _ => {}
            }
        }
        let ctx = egui::Context::default();
        let t = table(&builtin_definitions());
        // The grid measures itself on its first frame: read the second.
        let mut painted = Vec::new();
        for _ in 0..2 {
            let out = crate::headless::run_ui(&ctx, Default::default(), |ui| {
                body(ui, &t, "esp32c3");
            });
            painted.clear();
            out.shapes
                .iter()
                .for_each(|s| texts(&s.shape, &mut painted));
        }
        let color_of = |text: &str| {
            painted
                .iter()
                .find(|(t, _)| t == text)
                .map(|(_, c)| *c)
                .unwrap_or_else(|| panic!("{text:?} not painted"))
        };
        for label in std::iter::once("Chip").chain(Column::ALL.map(Column::label)) {
            color_of(label);
        }
        for row in &t.generated {
            color_of(&row.name);
        }
        assert_eq!(
            color_of("ESP32-C3"),
            GENERATED,
            "the project's chip is marked"
        );
        assert_ne!(color_of("ESP32-C6"), GENERATED);
        color_of("Generated on no chip:");
        for (_, line) in &t.never {
            color_of(line);
        }
    }
}
