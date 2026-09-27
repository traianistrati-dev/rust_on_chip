//! Serial monitor tab — a raw USART/UART console (Phase 1).
//!
//! Port + baud selectors, Connect/Disconnect, a live RX view (text / hex) and a
//! TX line. Reads/writes the host serial port the firmware's USART is wired to
//! (USB-UART bridge / on-board VCP), so data can be seen without an external
//! terminal. See [`crate::serial::SerialMonitor`].

use crate::serial::{
    SEARCH_HIT, SEARCH_HIT2, SerialMonitor, SerialView, byte_color, frame_ranges, gap_counts,
    hex_layout_job, hex_search_job, parse_hex_search, render_rx_text, seq_color, seq_counts,
    text_search_job,
};
use eframe::egui;
use egui_phosphor::regular as ph;

/// The shared baud picker and its typed-rate limits.
use crate::serial::{BAUD_MAX, BAUD_MIN, baud_picker};

/// Height of the send-area resize handle / minimum send-area height.
const HANDLE_H: f32 = 6.0;
const MIN_TX: f32 = 26.0;

/// Why the selected port cannot be opened, and the only two ways out.
///
/// Assembled in pieces rather than as one continued literal: a backslash
/// continuation renders with a run of spaces the moment rustfmt reflows it, and
/// this string is read by a user who is already confused about why nothing
/// connects.
///
/// Bridge is ruled out by name on purpose. It is the feature for "another
/// application holds this port", so it is exactly what a user reaches for here
/// yet `SerialMonitor::connect_bridge` opens the device port FIRST and fails
/// identically. Better to say so than to let them find out.
fn held_port_note(port: &str, who: &str) -> String {
    let mut s = format!("{who} is on {port}. ");
    s.push_str("A serial port has one owner, so this cannot open it too \u{2014} ");
    s.push_str("stop it in the Flash tab, or turn off its auto-start after flashing. ");
    s.push_str("Bridge is not a way around it: the bridge opens the same device port.");
    s
}

pub fn show_serial_tab(
    ui: &mut egui::Ui,
    serial: &mut SerialMonitor,
    ctx: &egui::Context,
    // A port another part of the IDE is holding open right now, as
    // (port, holder). Today only the ESP Monitor can be that holder, and it
    // starts itself after every ESP flash - so on an Espressif project the
    // default path lands the user here with the port already taken. A serial
    // port has exactly one owner, and without this the open failed with the
    // bare OS error ("Access is denied."), naming neither the cause nor the
    // way out.
    held_port: Option<(&str, &str)>,
) {
    if serial.ports_never_scanned() {
        serial.refresh_ports();
    }
    // Tells the reader threads this tab is on screen, so they keep repainting
    // (`terminal::drawn_recently`).
    serial.state.lock().unwrap().drawn_pass = ctx.cumulative_pass_nr_for(egui::ViewportId::ROOT);
    let connected = serial.is_connected();
    // Matched on the SELECTED port, not on the holder alone: another board on
    // another port is not a conflict, and saying so would be noise.
    let held_note = held_port
        .filter(|(port, _)| !port.is_empty() && *port == serial.port)
        .map(|(port, who)| held_port_note(port, who));

    // ── Controls row ──────────────────────────────────────────────────────────
    ui.horizontal_wrapped(|ui| {
        ui.label("Port:");
        // Each row carries what is on the other end, not just the number. An
        // Espressif board routinely enumerates TWO ports - the chip's own
        // USB-Serial/JTAG beside a CP210x/CH340 bridge - and the number alone
        // says nothing about which one is the chip. See `serial::port_label`.
        let selected_is = serial.port_labels.get(&serial.port).cloned();
        egui::ComboBox::from_id_salt("serial_port")
            .selected_text(if serial.port.is_empty() {
                "—".to_owned()
            } else {
                serial.port.clone()
            })
            .show_ui(ui, |ui| {
                for p in serial.ports.clone() {
                    let row = match serial.port_labels.get(&p) {
                        Some(what) => format!("{p} — {what}"),
                        None => p.clone(),
                    };
                    ui.selectable_value(&mut serial.port, p, row);
                }
            })
            .response
            .on_hover_text(match &selected_is {
                Some(what) => format!("{} — {what}", serial.port),
                None => "Which serial port to open".to_owned(),
            });
        if ui
            .button(ph::ARROWS_CLOCKWISE)
            .on_hover_text("Refresh ports")
            .clicked()
        {
            serial.refresh_ports();
        }

        ui.add_space(8.0);
        ui.label("Baud:");
        // The module panel's picker, so a custom rate seeded from a USART
        // module stays selectable here. No per-rate tags: this end is the
        // host's adapter, whose clock nothing here knows.
        baud_picker(
            ui,
            "serial_baud",
            &mut serial.baud,
            BAUD_MIN..=BAUD_MAX,
            &|_| None,
        );

        ui.add_space(8.0);
        if connected {
            if ui
                .button(
                    egui::RichText::new(format!("{} Disconnect", ph::PLUGS))
                        .color(egui::Color32::from_rgb(230, 150, 140)),
                )
                .clicked()
            {
                serial.disconnect();
            }
        } else {
            let can = !serial.port.is_empty()
                && (!serial.bridge || !serial.bridge_port.is_empty())
                && held_note.is_none();
            if ui
                .add_enabled(
                    can,
                    egui::Button::new(format!("{} Connect", ph::PLUGS_CONNECTED)),
                )
                .on_disabled_hover_text(if let Some(note) = &held_note {
                    note.clone()
                } else if serial.bridge {
                    "Bridge needs BOTH a device port and a virtual-pair port".to_owned()
                } else {
                    "Pick a port first".to_owned()
                })
                .clicked()
            {
                if serial.bridge {
                    serial.connect_bridge(ctx);
                } else {
                    serial.connect(ctx);
                }
            }
        }

        // The state itself, always on screen: which port at which baud is open,
        // or that none is. The button alone only says what would happen next.
        let (dot, text, color) = if connected {
            (
                ph::CHECK_CIRCLE,
                format!("{} @ {}", serial.port, serial.baud),
                egui::Color32::from_rgb(90, 200, 120),
            )
        } else {
            (
                ph::CIRCLE,
                "not connected".to_owned(),
                egui::Color32::from_gray(130),
            )
        };
        ui.label(
            egui::RichText::new(format!("{dot} {text}"))
                .size(10.5)
                .color(color),
        );

        ui.separator();
        // Bridge (MITM): relay a port another application already holds, instead
        // of opening it. Locked while connected — the wiring can't be re-pointed
        // under a live relay.
        ui.add_enabled_ui(!connected, |ui| {
            ui.checkbox(&mut serial.bridge, "Bridge")
                .on_hover_text(
                    "Man-in-the-middle a port another application is using.\n\
                     The app talks to a virtual port, the IDE relays every byte \
                     to the real device and logs both directions.",
                )
                .on_disabled_hover_text("Disconnect first to change the wiring");
        });
        // The explainer stays reachable whether or not Bridge is on — it is
        // what you read to decide whether you need Bridge at all.
        ui.toggle_value(&mut serial.info_on, format!("{} Info", ph::INFO))
            .on_hover_text("How Bridge (MITM) wiring works, with this session's ports");
    });

    // ── View row ──────────────────────────────────────────────────────────────
    // One `Type` picker instead of three checkboxes that could contradict each
    // other, and only the options that MEAN something for the chosen view: a
    // control that does nothing in the current mode is worse than a missing one,
    // because it invites the click.
    let view = serial.view();
    ui.horizontal_wrapped(|ui| {
        ui.label("Type:");
        let mut chosen = view;
        egui::ComboBox::from_id_salt("serial_view_type")
            .selected_text(view.label())
            .width(90.0)
            .show_ui(ui, |ui| {
                for v in [
                    SerialView::Raw,
                    SerialView::Matrix,
                    SerialView::Frames,
                    SerialView::Plot,
                ] {
                    ui.selectable_value(&mut chosen, v, v.label());
                }
            })
            .response
            .on_hover_text(
                "Default — the raw stream, text or coloured hex.\n\
                 Matrix  — the newest framed payload as a grid of numbers.\n\
                 Frames  — one row per protocol frame.\n\
                 Plot    — numeric lines as live curves.",
            );
        if chosen != view {
            serial.set_view(chosen);
        }

        // Plot brings its own controls; everything here would be dead weight.
        if view == SerialView::Plot {
            return;
        }

        ui.separator();
        ui.checkbox(&mut serial.hex, "Hex")
            .on_hover_text("Show bytes as hex instead of decoded text.");

        // Time is a RAW-view thing: Matrix and Frames already carry their own
        // per-frame timing.
        if view == SerialView::Raw {
            ui.checkbox(&mut serial.stamps, "Time").on_hover_text(
                "Show what was SENT and what was RECEIVED as timestamped blocks:\n\
                 >> what this console sent   ·   << what the device answered\n\
                 The `(+N ms)` on a reply is the time since the previous block — the \
                 send→receive latency.\n\n\
                 The clock is when the IDE wrote/read the bytes, not when they hit the \
                 wire: good to milliseconds, not better. Blocks are split by the idle \
                 gap set in Bridge mode.",
            );
            // Seq / Row lay out the flat hex dump — the timed view renders
            // blocks instead, where neither applies.
            if !serial.stamps {
                ui.add_enabled_ui(serial.hex, |ui| {
                    ui.label("Seq:");
                    ui.add(
                        crate::panels::drag_value(ui, &mut serial.seq_len)
                            .range(1..=16)
                            .speed(0.1),
                    )
                    .on_hover_text("Bytes per repeating sequence: each group of N bytes\nis coloured as a unit (same sequence -> same colour).");
                    ui.label("Row:");
                    ui.add(
                        crate::panels::drag_value(ui, &mut serial.row_bytes)
                            .range(1..=64)
                            .speed(0.2),
                    )
                    .on_hover_text("Bytes shown per line in the hex view.");
                });
            }
        }

        // Search fields. Field 1 works in BOTH views: hex mode highlights the
        // byte sequence in yellow; text mode tints whole LINES that START with
        // the typed text. Field 2 stays hex-only.
        ui.colored_label(SEARCH_HIT, "Find start:");
        ui.add(
            egui::TextEdit::singleline(&mut serial.search)
                .hint_text(if serial.hex { "hex e.g. 0D 0A" } else { "line prefix" })
                .desired_width(110.0),
        )
        .on_hover_text(
            "Hex view: highlight this hex sequence in yellow (rest greyed).\n\
             Text view: lines STARTING with this text turn yellow.",
        );

        // ── Payload size between the two markers ──────────────────────────
        // How many bytes sit BETWEEN Find1 and Find2 (both excluded) — the
        // payload length of each framed message. Hex mode only: that's where
        // both Find fields are byte sequences.
        if serial.hex {
            let a = parse_hex_search(&serial.search);
            let b = parse_hex_search(&serial.search2);
            if !a.is_empty() && !b.is_empty() {
                let gaps = {
                    let st = serial.state.lock().unwrap();
                    gap_counts(&st.rx, &a, &b)
                };
                ui.label(
                    egui::RichText::new("Between:")
                        .size(11.0)
                        .color(egui::Color32::GRAY),
                );
                let (text, color) = match (gaps.last(), gaps.iter().min(), gaps.iter().max()) {
                    (Some(&last), Some(&min), Some(&max)) => (
                        if min == max {
                            format!("{last} B")
                        } else {
                            // Sizes vary across frames — show the spread too.
                            format!("{last} B  ({min}..{max})")
                        },
                        egui::Color32::from_rgb(120, 210, 140),
                    ),
                    _ => ("—".to_owned(), egui::Color32::from_gray(120)),
                };
                ui.label(egui::RichText::new(text).size(11.0).monospace().color(color))
                    .on_hover_text(if gaps.is_empty() {
                        "Bytes between Find start and Find end, both markers excluded.\n\
                         No complete Find start … Find end pair in the buffer yet."
                            .to_owned()
                    } else {
                        format!(
                            "Bytes between Find start and Find end, both markers excluded \
                             (the payload of each framed message).\n\
                             {} frame(s) · last {} B · min {} B · max {} B",
                            gaps.len(),
                            gaps.last().copied().unwrap_or(0),
                            gaps.iter().min().copied().unwrap_or(0),
                            gaps.iter().max().copied().unwrap_or(0),
                        )
                    });
            }
        }

        ui.add_enabled_ui(serial.hex, |ui| {
            ui.colored_label(SEARCH_HIT2, "Find end:");
            ui.add(
                egui::TextEdit::singleline(&mut serial.search2)
                    .hint_text("hex e.g. 4F 4E")
                    .desired_width(110.0),
            )
            .on_hover_text("Highlight this hex sequence in blue (rest greyed).");
        });
        // Autoscroll and Clear belong to a SCROLLING stream. The Matrix shows
        // one payload and the Frames list keeps its own tail — neither has a
        // stream to pin or wipe from here.
        if view == SerialView::Raw {
            ui.checkbox(&mut serial.autoscroll, "Autoscroll");
            if ui
                .button(format!("{} Clear", ph::BROOM))
                .on_hover_text("Drop the received bytes and the timed blocks.")
                .clicked()
            {
                serial.clear_rx();
            }
        }
    });

    // Ahead of the click, not after it: the button is already disabled, and a
    // hover text nobody hovers explains nothing.
    if let Some(note) = &held_note {
        ui.colored_label(
            egui::Color32::from_rgb(220, 170, 90),
            format!("{} {note}", ph::WARNING),
        );
    }
    if let Some(err) = serial.state.lock().unwrap().error.clone() {
        ui.colored_label(
            egui::Color32::from_rgb(220, 90, 80),
            format!("{} {err}", ph::WARNING),
        );
    }
    // ── Bridge wiring row (only while Bridge is on) ─────────────────────────
    if serial.bridge {
        show_bridge_row(ui, serial, connected);
    }
    ui.separator();

    // ── RX view (fills the space left above the resizable send area) ────────────
    let section_h = ui.available_height();
    // Keep the send area valid for the current panel height (≥ Send button, and
    // leaving ≥ 40px for the RX view).
    let max_tx = (section_h - HANDLE_H - 40.0).max(MIN_TX);
    serial.tx_height = serial.tx_height.clamp(MIN_TX, max_tx);
    let rx_height = (section_h - serial.tx_height - HANDLE_H).max(40.0);

    // ── Plot / Matrix view (replaces the text/hex view while on; the send
    //    area below keeps working, so commands can be sent meanwhile) ─────────
    if serial.info_on {
        // Outranks every other view: it was asked for explicitly, and it is
        // read while nothing is connected.
        let (app_side, ide_side) = match &serial.pair {
            Some(p) => (p.app_side.clone(), p.ide_side.clone()),
            None => (String::new(), serial.bridge_port.clone()),
        };
        super::serial_info::show_bridge_info(
            ui,
            section_h,
            &serial.port,
            &app_side,
            &ide_side,
            !cfg!(windows),
        );
        return;
    }
    if serial.matrix.on {
        // Newest complete Find-start…Find-end payload + how many the buffer
        // holds (the counter makes a live stream visibly tick).
        let (payload, frames_total) = {
            let a = parse_hex_search(&serial.search);
            let b = parse_hex_search(&serial.search2);
            if a.is_empty() || b.is_empty() {
                (None, 0)
            } else {
                let st = serial.state.lock().unwrap();
                let ranges = frame_ranges(&st.rx, &a, &b);
                (
                    ranges.last().map(|&(s, e)| st.rx[s..e].to_vec()),
                    ranges.len(),
                )
            }
        };
        crate::serial_matrix::show_matrix(
            ui,
            &mut serial.matrix,
            payload.as_deref(),
            frames_total,
            rx_height,
        );
    } else if serial.bridge {
        // The bridge log takes the WHOLE section: in relay mode the IDE is not
        // a participant, so there is nothing to send and no send area to leave
        // room for. Injecting bytes is a deliberate non-feature — it would
        // corrupt a conversation the user came here to observe.
        show_bridge_log(ui, serial, section_h);
        return;
    } else if serial.plot_on {
        {
            let st = serial.state.lock().unwrap();
            serial.plot.feed(&st.rx, st.rx_total);
        }
        crate::serial_plot::show_plot(ui, &mut serial.plot, rx_height);
    } else {
        show_rx_view(ui, serial, rx_height);
    }

    // ── Send area (drag handle + TX line) — shared by both views ────────────────
    show_tx_area(ui, serial, ctx, max_tx);
}

/// The Bridge wiring row: which real device, which end of the virtual pair, and
/// — the part everyone gets stuck on — what the OTHER application must open.
fn show_bridge_row(ui: &mut egui::Ui, serial: &mut SerialMonitor, connected: bool) {
    use crate::serial_bridge::{PairProvider, provider, setup_hint};
    ui.add_enabled_ui(!connected, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Pair:").strong());
            match provider() {
                // Unix: the IDE can make the pair itself, so it does.
                PairProvider::Socat => {
                    if ui
                        .button(format!("{} Create pair", ph::PLUS))
                        .on_hover_text("Run socat to create two linked PTYs")
                        .clicked()
                    {
                        serial.create_pair();
                    }
                    if !serial.bridge_port.is_empty() {
                        ui.label(
                            egui::RichText::new(&serial.bridge_port)
                                .monospace()
                                .size(11.0),
                        );
                    }
                }
                // Windows: the pair is a driver resource the user made earlier,
                // but the IDE can LOOK IT UP — asking someone to remember which
                // two COM numbers are mates is the part that goes wrong.
                PairProvider::Com0com => {
                    let pairs = serial.com0com_pairs.clone();
                    let label = match &serial.pair {
                        Some(p) => format!("{} <-> {}", p.ide_side, p.app_side),
                        None => "—".to_owned(),
                    };
                    egui::ComboBox::from_id_salt("bridge_pair_port")
                        .selected_text(label)
                        .show_ui(ui, |ui| {
                            if pairs.is_empty() {
                                ui.label(
                                    egui::RichText::new("no com0com pair detected")
                                        .size(10.5)
                                        .italics(),
                                );
                            }
                            for (a, b) in &pairs {
                                // The IDE takes B, the other app gets A — an
                                // arbitrary but STABLE split; Swap flips it.
                                if ui.selectable_label(false, format!("{a} <-> {b}")).clicked() {
                                    serial.bridge_port = b.clone();
                                    serial.pair =
                                        Some(crate::serial_bridge::VirtualPair::existing(
                                            b.clone(),
                                            a.clone(),
                                        ));
                                }
                            }
                        });
                    if serial.pair.is_some()
                        && ui
                            .button(ph::ARROWS_LEFT_RIGHT)
                            .on_hover_text("Swap which end of the pair the IDE holds")
                            .clicked()
                    {
                        if let Some(p) = serial.pair.take() {
                            let swapped = crate::serial_bridge::VirtualPair::existing(
                                p.app_side.clone(),
                                p.ide_side.clone(),
                            );
                            serial.bridge_port = swapped.ide_side.clone();
                            serial.pair = Some(swapped);
                        }
                    }
                }
            }
        });
    });
    let hint = setup_hint(serial.pair.as_ref());
    ui.label(
        egui::RichText::new(hint)
            .size(10.5)
            .color(egui::Color32::from_rgb(150, 160, 180)),
    );
}

/// The relayed traffic, newest at the bottom: `>>` app→device, `<<` device→app.
fn show_bridge_log(ui: &mut egui::Ui, serial: &mut SerialMonitor, height: f32) {
    use crate::serial::{DIR_APP, DIR_SENSOR, bridge_log_job};
    ui.horizontal(|ui| {
        ui.colored_label(DIR_APP, ">> app -> device");
        ui.add_space(10.0);
        ui.colored_label(DIR_SENSOR, "<< device -> app");
        ui.add_space(10.0);
        if ui.button("Clear").clicked() {
            serial.state.lock().unwrap().log.clear();
        }
        ui.add_space(10.0);
        ui.checkbox(&mut serial.stamps, "Time").on_hover_text(
            "Prefix each block with its wall clock and the gap since the previous 
             one. The time is when the IDE READ the bytes, not when they hit the 
             wire - good to milliseconds, not better.",
        );
        // The block boundary is a guess about the protocol, so it has to be
        // adjustable while watching the traffic.
        let mut gap = serial.block_gap_ms();
        ui.label("Gap:");
        let resp = ui.add(
            crate::panels::drag_value(ui, &mut gap)
                .range(1..=2000)
                .speed(1.0)
                .suffix(" ms"),
        );
        if resp.changed() {
            serial.set_block_gap_ms(gap);
        }
        resp.on_hover_text(
            "Silence that ends a block. Bytes arriving closer than this join the 
             block in progress - a frame delivered in several reads stays one 
             block. Raise it if frames get split, lower it if they run together.
             A block also ends at 16 KB whatever the gap, so a stream that never
             pauses stays bounded.",
        );
        // Say so when the view is filtered — an empty log because a filter is
        // on looks exactly like an empty log because nothing is happening.
        if !serial.search.is_empty() || !serial.search2.is_empty() {
            ui.add_space(10.0);
            ui.colored_label(
                SEARCH_HIT,
                format!(
                    "{} filtered to bursts containing Find start / Find end",
                    ph::FUNNEL
                ),
            );
        }
    });
    // The Find fields mean the same thing here as in the RX view, read in the
    // mode you are in: hex mode parses them as byte sequences, text mode takes
    // the typed characters as-is. Same field, no second concept to learn.
    let (a, b) = if serial.hex {
        (
            parse_hex_search(&serial.search),
            parse_hex_search(&serial.search2),
        )
    } else {
        (
            serial.search.as_bytes().to_vec(),
            serial.search2.as_bytes().to_vec(),
        )
    };
    let job = {
        let st = serial.state.lock().unwrap();
        // Bridge: Find FILTERS — the point there is to pull one frame out of
        // someone else's conversation.
        bridge_log_job(
            &st.log,
            serial.hex,
            12.0,
            &a,
            &b,
            serial.stamps,
            st.epoch,
            true,
        )
    };
    egui::ScrollArea::both()
        .id_salt("bridge_log")
        .max_height(height - 24.0)
        .stick_to_bottom(serial.autoscroll)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.label(job);
        });
}

/// One row per protocol frame, with the framing rules above the list.
///
/// The Find fields double as the marker patterns — they already hold the header
/// and tail the user typed to measure the payload, so asking for them twice
/// would be asking the same question twice.
fn show_frames_view(ui: &mut egui::Ui, serial: &mut SerialMonitor, rx_height: f32) {
    use crate::serial_frames::{FrameMode, frames_from_log, frames_summary};

    // Framing controls.
    ui.horizontal_wrapped(|ui| {
        ui.label("Framing:");
        ui.selectable_value(&mut serial.frame_spec.mode, FrameMode::Markers, "Markers")
            .on_hover_text(
                "A frame runs from the Find start pattern to the Find end pattern.\n\
                 Simple and exact when the protocol has a unique tail.",
            );
        ui.selectable_value(&mut serial.frame_spec.mode, FrameMode::Length, "Length")
            .on_hover_text(
                "A frame starts at the Find start pattern and ends where its own length \
                 field says.\nThe only option without a unique tail — and the only one \
                 that can tell a TRUNCATED frame from a short one.",
            );
        ui.separator();
        if serial.frame_spec.mode == FrameMode::Length {
            ui.label("len@");
            ui.add(
                crate::panels::drag_value(ui, &mut serial.frame_spec.len_offset)
                    .range(0..=64)
                    .speed(0.1),
            )
            .on_hover_text(
                "Byte offset of the length field, counted from the header's FIRST byte.",
            );
            ui.label("width");
            ui.add(
                crate::panels::drag_value(ui, &mut serial.frame_spec.len_width)
                    .range(1..=4)
                    .speed(0.05),
            )
            .on_hover_text("Length field width in bytes.");
            ui.checkbox(&mut serial.frame_spec.len_le, "LE")
                .on_hover_text("Little-endian length field (off = big-endian).");
            ui.label("tail");
            ui.add(
                crate::panels::drag_value(ui, &mut serial.frame_spec.tail_len)
                    .range(0..=64)
                    .speed(0.1),
            )
            .on_hover_text(
                "Bytes AFTER the counted length — a trailer or checksum the length \
                 field doesn't include.",
            );
            ui.checkbox(
                &mut serial.frame_spec.len_covers_header,
                "len covers header",
            )
            .on_hover_text(
                "On: the length counts from the header's first byte. Off: it counts \
                     only what follows the field. Datasheets use both — the wrong one \
                     shifts every frame.",
            );
        }
    });

    // The Find fields ARE the markers.
    serial.frame_spec.start = parse_hex_search(&serial.search);
    serial.frame_spec.end = parse_hex_search(&serial.search2);

    let (frames, epoch) = {
        let st = serial.state.lock().unwrap();
        (frames_from_log(&st.log, &serial.frame_spec), st.epoch)
    };
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(frames_summary(&frames))
                .size(10.5)
                .color(egui::Color32::from_gray(150)),
        );
        if serial.frame_spec.start.is_empty() {
            ui.label(
                egui::RichText::new("— set Find start to the frame header")
                    .size(10.5)
                    .color(egui::Color32::from_rgb(210, 170, 90)),
            );
        } else {
            ui.label(
                egui::RichText::new("· click a row to open it in the Matrix")
                    .size(10.0)
                    .color(egui::Color32::from_gray(120)),
            );
        }
    });

    // One clickable widget per row: clicking sends THAT frame's payload to the
    // Matrix view, which then holds it (the matrix normally follows the newest
    // frame — the opposite of what you want after singling one out).
    let mut open_in_matrix: Option<Vec<u8>> = None;
    let start = frames.len().saturating_sub(crate::serial_frames::MAX_ROWS);
    egui::ScrollArea::both()
        .id_salt("serial_frames_view")
        .stick_to_bottom(serial.autoscroll)
        .auto_shrink([false, false])
        .max_height(rx_height - 22.0)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            let mut prev: Option<std::time::Instant> = None;
            for (n, f) in frames.iter().enumerate().skip(start) {
                let job = crate::serial_frames::frame_row_job(
                    f,
                    n,
                    prev,
                    serial.hex,
                    12.0,
                    &serial.frame_spec.start,
                    &serial.frame_spec.end,
                    epoch,
                );
                prev = Some(f.at);
                let resp = ui.add(
                    egui::Label::new(job)
                        .selectable(false)
                        .sense(egui::Sense::click()),
                );
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                let resp = resp.on_hover_text(
                    "Open this frame in the Matrix view (it freezes there — press \
                     Pause in the Matrix to follow the stream again).",
                );
                if resp.clicked() {
                    // The payload BETWEEN the markers is what the matrix decodes;
                    // sending the header and tail too would shift every value.
                    let a = serial.frame_spec.start.len();
                    let b = serial.frame_spec.end.len();
                    let body = if f.kind == crate::serial_frames::FrameKind::Complete
                        && f.bytes.len() >= a + b
                    {
                        &f.bytes[a..f.bytes.len() - b]
                    } else {
                        &f.bytes[..]
                    };
                    open_in_matrix = Some(body.to_vec());
                }
            }
        });
    if let Some(payload) = open_in_matrix {
        serial.matrix.show_payload(&payload);
        serial.frames_on = false; // the matrix takes the RX area
    }
}

/// The classic RX view: coloured hex (+ unique-sequences legend) or decoded
/// text, with the Find highlights. Extracted unchanged so the Plot toggle can
/// swap it for the live plotter.
fn show_rx_view(ui: &mut egui::Ui, serial: &mut SerialMonitor, rx_height: f32) {
    // ── Timed view ────────────────────────────────────────────────────────────
    // With "Time" on, the console shows the same block log the Bridge does —
    // BOTH directions, each stamped, with the gap since the previous block. That
    // gap on a `<<` line right after a `>>` line IS the send→receive latency,
    // which the raw byte stream cannot express: it has no notion of when
    // anything arrived, or of who said it.
    // ── Frames view ───────────────────────────────────────────────────────────
    // One row per protocol frame. Checked before the timed view: framing is a
    // stronger statement about the stream than "split it where it went quiet".
    if serial.frames_on {
        show_frames_view(ui, serial, rx_height);
        return;
    }

    if serial.stamps {
        let job = {
            let st = serial.state.lock().unwrap();
            crate::serial::bridge_log_job(
                &st.log,
                serial.hex,
                12.0,
                &parse_hex_search(&serial.search),
                &parse_hex_search(&serial.search2),
                true,
                st.epoch,
                // Find HIGHLIGHTS here, it does not filter — same as the plain
                // hex view. Keeping only matching blocks would hide the reply
                // whose latency you are reading, and would blank the pane
                // entirely while a pattern matches nothing yet.
                false,
            )
        };
        egui::ScrollArea::both()
            .id_salt("serial_timed_log")
            .stick_to_bottom(serial.autoscroll)
            .auto_shrink([false, false])
            .max_height(rx_height)
            .show(ui, |ui| {
                ui.add(egui::Label::new(job).selectable(true));
            });
        return;
    }

    // Build the display under one lock. Search mode → yellow/grey highlight (no
    // legend); hex mode → per-sequence colours + unique-sequences legend; text
    // mode → plain decoded text.
    let hex = serial.hex;
    let seq_len = serial.seq_len.max(1);
    let search_a = parse_hex_search(&serial.search);
    let search_b = parse_hex_search(&serial.search2);
    let searching = hex && (!search_a.is_empty() || !search_b.is_empty());
    let mut patterns: Vec<(&[u8], egui::Color32)> = Vec::new();
    if !search_a.is_empty() {
        patterns.push((&search_a, SEARCH_HIT));
    }
    if !search_b.is_empty() {
        patterns.push((&search_b, SEARCH_HIT2));
    }
    let (hex_job, text_display, counts) = {
        let st = serial.state.lock().unwrap();
        if hex {
            // Search highlight (yellow/blue) when a Find field is filled, else
            // the per-sequence colouring. The unique-sequence legend is always
            // computed so it stays visible even while searching.
            let job = if searching {
                hex_search_job(&st.rx, 12.0, &patterns, serial.row_bytes)
            } else {
                hex_layout_job(&st.rx, 12.0, seq_len, serial.row_bytes)
            };
            (Some(job), String::new(), seq_counts(&st.rx, seq_len))
        } else {
            (None, render_rx_text(&st.rx), Vec::new())
        }
    };

    if let Some(job) = hex_job {
        // Coloured hex on the left, unique-sequences legend on the right with a
        // draggable vertical divider. All bounded to `rx_height` so the resize
        // handle + send area below stay visible.
        const DIV_W: f32 = 6.0;
        let max_legend = (ui.available_width() - 160.0).max(80.0);
        serial.legend_w = serial.legend_w.clamp(80.0, max_legend);
        let legend_w = serial.legend_w;
        let hex_w = (ui.available_width() - legend_w - DIV_W - 8.0).max(120.0);
        ui.horizontal_top(|ui| {
            egui::ScrollArea::both()
                .id_salt("serial_rx_hex")
                .stick_to_bottom(serial.autoscroll)
                .auto_shrink([false, false])
                .max_height(rx_height)
                .max_width(hex_w)
                .show(ui, |ui| {
                    ui.add(egui::Label::new(job).selectable(true));
                });

            // Draggable vertical divider — resize the legend width.
            let (div_rect, _) =
                ui.allocate_exact_size(egui::vec2(DIV_W, rx_height), egui::Sense::hover());
            let div = ui.interact(
                div_rect,
                ui.id().with("serial_legend_resize"),
                egui::Sense::drag(),
            );
            let div_color = if div.hovered() || div.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                egui::Color32::from_rgb(100, 140, 200)
            } else {
                egui::Color32::from_gray(70)
            };
            let cx = div_rect.center().x;
            ui.painter().vline(
                cx,
                div_rect.y_range(),
                egui::Stroke::new(1.5_f32, div_color),
            );
            for dy in [-6.0_f32, 0.0, 6.0] {
                ui.painter().circle_filled(
                    egui::pos2(cx, div_rect.center().y + dy),
                    1.5,
                    div_color,
                );
            }
            if div.dragged() {
                // Drag left → legend grows; right → shrinks.
                serial.legend_w = (serial.legend_w - div.drag_delta().x).clamp(80.0, max_legend);
            }

            egui::ScrollArea::both()
                .id_salt("serial_legend")
                .auto_shrink([false, false])
                .max_height(rx_height)
                .max_width(legend_w)
                .show(ui, |ui| {
                    // Force a vertical list — the scroll area inherits the parent
                    // `horizontal_top` layout, which would otherwise flow the rows
                    // left-to-right.
                    ui.vertical(|ui| {
                        let title = if seq_len == 1 {
                            "Unique bytes".to_owned()
                        } else {
                            format!("Unique {seq_len}-byte seq")
                        };
                        ui.label(
                            egui::RichText::new(title)
                                .size(10.0)
                                .color(egui::Color32::GRAY),
                        );
                        for (seq, count) in counts.iter().take(96) {
                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(11.0, 11.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(rect, 2.0, seq_color(seq));
                                let hex: String = seq
                                    .iter()
                                    .map(|b| format!("{b:02X}"))
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                let ascii: String = seq
                                    .iter()
                                    .map(|&b| {
                                        if (0x20..0x7f).contains(&b) {
                                            b as char
                                        } else {
                                            '·'
                                        }
                                    })
                                    .collect();
                                ui.label(
                                    egui::RichText::new(format!("{hex}  {ascii} ×{count}"))
                                        .monospace()
                                        .size(11.0),
                                );
                            });
                        }
                    });
                });
        });
    } else {
        let needle = serial.search.trim().to_owned();
        egui::ScrollArea::vertical()
            .id_salt("serial_rx_text")
            .stick_to_bottom(serial.autoscroll)
            .auto_shrink([false, false])
            .max_height(rx_height)
            .show(ui, |ui| {
                if needle.is_empty() {
                    ui.add(
                        egui::Label::new(egui::RichText::new(text_display).monospace().size(12.0))
                            .selectable(true)
                            .wrap(),
                    );
                } else {
                    // Find-1 in text mode: whole lines STARTING with the
                    // needle turn yellow, the rest keep the default colour.
                    let job =
                        text_search_job(&text_display, &needle, 12.0, ui.visuals().text_color());
                    ui.add(egui::Label::new(job).selectable(true).wrap());
                }
            });
    }
}

/// The resizable send area: drag handle, TX text box (hex-coloured in hex
/// mode), Send + CR+LF + line-gap pacing. Shared by the RX and Plot views.
fn show_tx_area(ui: &mut egui::Ui, serial: &mut SerialMonitor, ctx: &egui::Context, max_tx: f32) {
    let hex = serial.hex;
    let connected = serial.is_connected();

    // ── Drag handle — resize the send area up / down ────────────────────────────
    let (handle_rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), HANDLE_H),
        egui::Sense::hover(),
    );
    let drag = ui.interact(
        handle_rect,
        ui.id().with("serial_tx_resize"),
        egui::Sense::drag(),
    );
    let line_color = if drag.hovered() || drag.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        egui::Color32::from_rgb(100, 140, 200)
    } else {
        egui::Color32::from_gray(70)
    };
    let mid_y = handle_rect.center().y;
    ui.painter().hline(
        handle_rect.x_range(),
        mid_y,
        egui::Stroke::new(1.5_f32, line_color),
    );
    for dx in [-6.0_f32, 0.0, 6.0] {
        ui.painter().circle_filled(
            egui::pos2(handle_rect.center().x + dx, mid_y),
            1.5,
            line_color,
        );
    }
    if drag.dragged() {
        // Dragging up (negative delta) grows the send area.
        serial.tx_height = (serial.tx_height - drag.drag_delta().y).clamp(MIN_TX, max_tx);
    }

    // ── Send area — Send + CR+LF pinned right (always visible); the text box
    //    fills the rest and is as tall as the (resizable) send area. ────────────
    let mut do_send = false;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.checkbox(&mut serial.append_crlf, "CR+LF");
        // Per-line pause (ms) for multi-line command sequences that need the
        // device to settle before the next one. 0 = send back-to-back.
        ui.add(
            crate::panels::drag_value(ui, &mut serial.line_delay_ms)
                .range(0..=60_000)
                .speed(10.0)
                .suffix(" ms"),
        )
        .on_hover_text("Pause between each line when sending a multi-line block");
        ui.label("line gap");
        if ui
            .add_enabled(
                connected,
                egui::Button::new(format!("{} Send", ph::PAPER_PLANE_RIGHT)),
            )
            .clicked()
        {
            do_send = true;
        }
        // In hex mode, colour the typed text per byte (same scheme as the RX
        // view) so repeated chars match the legend colours.
        let mut tx_layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
            let font = egui::FontId::monospace(13.0);
            let mut job = egui::text::LayoutJob::default();
            let s = buf.as_str();
            if hex {
                for c in s.chars() {
                    let col = if c.is_ascii() {
                        byte_color(c as u8)
                    } else {
                        egui::Color32::LIGHT_GRAY
                    };
                    job.append(
                        &c.to_string(),
                        0.0,
                        egui::text::TextFormat::simple(font.clone(), col),
                    );
                }
            } else {
                job.append(
                    s,
                    0.0,
                    egui::text::TextFormat::simple(font.clone(), egui::Color32::from_gray(220)),
                );
            }
            job.wrap.max_width = wrap;
            ui.fonts_mut(|f| f.layout_job(job))
        };
        let resp = ui.add_sized(
            [ui.available_width(), serial.tx_height],
            egui::TextEdit::multiline(&mut serial.tx_input)
                .hint_text(if hex {
                    "hex bytes e.g. 41 54 0D (Ctrl+Enter)"
                } else {
                    "text to send (Ctrl+Enter)"
                })
                .interactive(connected)
                .layouter(&mut tx_layouter),
        );
        // Ctrl+Enter sends; plain Enter inserts a newline (multi-line composing).
        if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command)
        {
            do_send = true;
        }
    });

    if do_send && connected && !serial.tx_input.trim_end_matches(['\r', '\n']).is_empty() {
        // Encode each non-empty line, then queue them so they go out one at a
        // time with the configured `line_gap` pause — non-blocking (paced by
        // `pump_tx_queue` below), so the UI stays responsive during the sequence.
        let mut bad: Option<String> = None;
        let lines: Vec<Vec<u8>> = serial
            .tx_input
            .clone()
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .filter_map(|line| match encode_tx_line(line, hex) {
                Ok(mut bytes) => {
                    if serial.append_crlf {
                        bytes.extend_from_slice(b"\r\n");
                    }
                    Some(bytes)
                }
                Err(e) => {
                    // Dropping it in silence was the old behaviour, and it is
                    // the worst of the three: the line simply did not go out.
                    bad.get_or_insert(e);
                    None
                }
            })
            .collect();
        if let Some(e) = bad {
            serial.state.lock().unwrap().error = Some(e);
        }
        serial.queue_lines(lines);
    }

    // Pace the pending TX queue (if any); schedule a repaint when the next line
    // is due so the pause elapses even while the app is otherwise idle.
    if let Some(due_in) = serial.pump_tx_queue() {
        ctx.request_repaint_after(due_in);
    }
}

/// One typed line into the bytes that go on the wire.
///
/// The **Hex** toggle decides - the same toggle that already colours this box
/// per byte and switches the RX view. It governed only the LOOK: the line was
/// hex-decoded unconditionally, with `unwrap_or_default()` swallowing the
/// failure, so a box whose hint read "text to send" sent nothing at all when
/// given text. `hello` parsed to no bytes, and a word that happens to be two
/// hex digits (`de`) went out as the single byte 0xDE. Text sending was there
/// originally (`tx_input.into_bytes()`) and was REPLACED, not extended, when
/// hex sequences were added.
fn encode_tx_line(line: &str, hex: bool) -> Result<Vec<u8>, String> {
    if !hex {
        return Ok(line.as_bytes().to_vec());
    }
    hex_string_to_bytes(line).map_err(|_| format!("Not hex bytes: {line:?} - nothing was sent"))
}

fn hex_string_to_bytes(s: &str) -> Result<Vec<u8>, std::num::ParseIntError> {
    s.split_whitespace()
        .map(|x| u8::from_str_radix(x, 16))
        .collect()
}

#[cfg(test)]
mod held_port_tests {
    use super::encode_tx_line;

    /// The send box obeys the Hex toggle it already renders itself by. Before
    /// this, text typed into a box hinting "text to send" produced NO bytes,
    /// and a two-hex-digit word produced the wrong one.
    #[test]
    fn the_send_box_encodes_the_way_its_own_toggle_says() {
        // Text mode: the bytes are the characters, verbatim.
        assert_eq!(encode_tx_line("AT+RST", false).unwrap(), b"AT+RST".to_vec());
        // The two cases the old unconditional hex decode got wrong.
        assert_eq!(encode_tx_line("hello", false).unwrap(), b"hello".to_vec());
        assert_eq!(encode_tx_line("de", false).unwrap(), b"de".to_vec());

        // Hex mode still parses whitespace-separated bytes, as before.
        assert_eq!(
            encode_tx_line("41 54 0D", true).unwrap(),
            vec![0x41, 0x54, 0x0D]
        );
        // And a line that is not hex now SAYS so instead of vanishing.
        let err = encode_tx_line("hello", true).unwrap_err();
        assert!(err.contains("hello"), "{err}");
    }

    use super::*;

    /// Only the SELECTED port is a conflict. This mirrors the `filter` in
    /// `show_serial_tab`, which is the whole decision.
    fn conflicts(selected: &str, held: &str) -> bool {
        !held.is_empty() && held == selected
    }

    #[test]
    fn only_the_selected_port_is_a_conflict() {
        assert!(conflicts("COM7", "COM7"));
        // A monitor watching another board is not this board's problem.
        assert!(!conflicts("COM7", "COM3"));
        // Idle monitor: `EspMonitor::active_port` returns "" when not busy, and
        // an empty holder must never match an empty selection either.
        assert!(!conflicts("COM7", ""));
        assert!(!conflicts("", ""));
    }

    /// The note names whichever holder actually has it.
    ///
    /// The first pass wired ONE holder, the ESP Monitor, and called the case
    /// covered. There are two: `espflash` takes the port for `flash` and for
    /// `board-info`, and it runs BEFORE the Monitor - so the window where the
    /// Serial tab failed with a bare "Access is denied" was the flash itself,
    /// which is exactly when a user reaches for the console.
    #[test]
    fn either_holder_is_named_by_name() {
        for (who, port) in [("espflash", "COM7"), ("The ESP Monitor", "COM7")] {
            let n = held_port_note(port, who);
            assert!(n.starts_with(who), "{n}");
            assert!(n.contains(port), "{n}");
            assert!(n.contains("one owner"), "{n}");
        }
    }

    /// `Building` holds no port - that phase is a cargo build. Treating it as a
    /// holder would refuse Connect for the minutes a build takes, on a port
    /// nothing has opened.
    #[test]
    fn the_build_phase_is_not_a_port_holder() {
        use crate::espflash::EspFlashState;
        for s in [EspFlashState::Flashing, EspFlashState::ReadingInfo] {
            assert!(s.is_busy(), "{s:?} must count as busy");
        }
        assert!(
            EspFlashState::Building.is_busy(),
            "Building is busy for the UI…"
        );
        // …but the panel excludes it from the HOLDER test on purpose. Encode
        // that here so the exclusion is not quietly dropped.
        let holds_port =
            |s: &EspFlashState| matches!(s, EspFlashState::Flashing | EspFlashState::ReadingInfo);
        assert!(!holds_port(&EspFlashState::Building));
        assert!(holds_port(&EspFlashState::Flashing));
        assert!(holds_port(&EspFlashState::ReadingInfo));
        assert!(!holds_port(&EspFlashState::Idle));
        assert!(!holds_port(&EspFlashState::Success));
    }

    #[test]
    fn the_note_names_the_holder_the_port_and_the_fix() {
        let n = held_port_note("COM7", "The ESP Monitor");
        assert!(n.contains("COM7"), "{n}");
        assert!(n.contains("The ESP Monitor"), "{n}");
        assert!(n.contains("Flash tab"), "names where to stop it: {n}");
        assert!(n.contains("auto-start"), "names the other way out: {n}");
    }

    /// The one wrong turn this message exists to head off: Bridge reads like
    /// the answer to "another program has my port", and is not — it opens the
    /// same device port. If that sentence is ever dropped, this fails.
    #[test]
    fn the_note_rules_bridge_out_rather_than_offering_it() {
        let n = held_port_note("COM7", "The ESP Monitor");
        assert!(n.contains("Bridge is not a way around it"), "{n}");
    }

    /// No stray run of spaces — the failure mode a continued literal produces
    /// once rustfmt reflows it, invisible in review and obvious in the UI.
    #[test]
    fn the_note_is_not_a_reflowed_continuation() {
        let n = held_port_note("COM7", "The ESP Monitor");
        assert!(!n.contains("  "), "double space in: {n}");
        assert!(!n.contains('\n'), "newline in: {n}");
    }
}
