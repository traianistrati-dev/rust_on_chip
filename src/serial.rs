//! Serial monitor — a built-in USART/UART console.
//!
//! Opens a host serial port (the USB-UART bridge / on-board VCP the firmware's
//! USART is wired to) and shows the bytes live, with a send line — so the data a
//! Virtual USART module exchanges can be read/written from inside the IDE
//! without an external terminal. Phase 1: a raw console (text / hex).
//!
//! A background thread owns a read handle and appends incoming bytes to the
//! shared [`SerialState`]; the UI keeps a `try_clone`d write handle for sending.

use eframe::egui;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Cap on the retained RX byte buffer (older bytes are dropped).
const RX_CAP: usize = 64_000;

/// Cap on the retained bridge log, in BYTES across all chunks.
const LOG_CAP: usize = RX_CAP;

/// Largest block a burst may JOIN into. Past it, the next burst starts a new
/// block even inside the gap.
///
/// Trimming drops whole blocks and keeps the last one, so a device that never
/// pauses longer than the gap — a 100 Hz sensor line, back-to-back binary
/// frames — used to build ONE block that no cap ever touched, and the Time view
/// laid all of it out on every frame. Capped, the log is bounded again; the cost
/// is that such a stream, or a single reply longer than this, shows as
/// consecutive stamped blocks. A quarter of the log, so an ordinary reply stays
/// one block. Framing is not affected: the Frames view joins each direction
/// before it looks for frames.
const MAX_BLOCK: usize = LOG_CAP / 4;

/// Which way a logged burst was travelling in Bridge mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    /// The other application → the real device.
    AppToSensor,
    /// The real device → the other application.
    SensorToApp,
}

/// Default idle gap that ends a block. Comfortably longer than the pauses
/// INSIDE a frame at common baud rates, comfortably shorter than the turnaround
/// between a command and its reply.
pub const DEFAULT_BLOCK_GAP_MS: u64 = 20;
/// The rates both baud pickers offer - the Serial tab's and the USART Virtual
/// Module's.
///
/// ONE list, because the two are joined: opening the Serial tab seeds its baud
/// from a USART module, so a rate one of them offers and the other does not is a
/// rate the user can be handed and then cannot re-select. They were two
/// hand-kept copies in two files with nothing comparing them.
///
/// 74880 is the odd one and is here for the Espressif parts. A board whose
/// crystal is 26 MHz boots with the ROM's UART divisor sized for 40 MHz, so its
/// boot log arrives at 115200 x 26 / 40, which is exactly 74880. Both pickers
/// take a typed rate now (see [`baud_picker`]), but this one stays a preset:
/// it is the rate a user needs exactly when they do not know why their board
/// prints garbage, so it has to be findable without knowing the number.
///
/// WHICH parts can be that board is read from the shipped definitions, not
/// asserted: `esp32` and `esp32c2` are the only two of the nine that offer
/// 26 MHz at all, and both offer 40 MHz as well
/// (`the_only_26mhz_parts_are_the_two_this_rate_is_for` pins the set in both
/// directions). 26 is NOT a hardware default for them - esp-hal pre-writes
/// `XtalClkConfig::_40` and then measures the real one - it is the low end of
/// the range, which is what the IDE picks for the Clock tab. So 74880 is the
/// rate for a 26 MHz BOARD, not for every ESP32; a stock 40 MHz WROOM logs at
/// 115200.
///
/// The other seven carry no 26 MHz option: `esp32c5` is 40 or 48 MHz, `esp32h2`
/// is 32 MHz and nothing else, and the remaining five are 40 MHz only. Whether
/// the H2 ROM divides for 40 (which would put its log at 115200 x 32 / 40 =
/// 92160) or knows its own crystal is not something this repo can check, so no
/// rate is invented for it here.
pub const BAUDS: [u32; 9] = [
    9600, 19200, 38400, 57600, 74880, 115200, 230400, 460800, 921600,
];

/// The slowest rate either picker takes as a typed value. Nothing the IDE
/// generates for is useful below it, and it keeps 0 - a divide by zero in two
/// of the HALs - out of the field.
pub const BAUD_MIN: u32 = 300;
/// The fastest typed rate: above every UART the IDE generates for. The real
/// ceiling is a fraction of the chip's clock, which the module panel works out
/// and REPORTS - see `uart_baud` - rather than enforcing it here.
pub const BAUD_MAX: u32 = 25_000_000;

/// Whether `baud` is one of the shared presets.
pub fn is_preset(baud: u32) -> bool {
    BAUDS.contains(&baud)
}

/// What a preset carries beside its number in the dropdown.
pub struct BaudTag {
    pub text: String,
    pub color: egui::Color32,
    pub hover: String,
}

/// The baud picker both panels draw: the presets, then "Custom…" for any
/// other rate.
///
/// ONE widget for the same reason [`BAUDS`] is one list: the Serial tab seeds
/// its rate from a USART module, so every rate a module can hold has to be one
/// the tab can show and re-select. A rate outside the presets is simply shown
/// in the typed field.
///
/// The typed field commits on Enter or when it loses focus, never per
/// keystroke: the module's rate is hashed into the generated config file, and
/// typing `250000` would otherwise write it at 2, 25, 250... baud on the way.
/// Dragging is off for the same reason. It does not clamp a stored value that
/// is already outside `range` either - a hand-edited `mcu.config` keeps its
/// number, and the caller reports it instead of the widget rewriting it.
///
/// `tag` annotates each preset (the module panel passes the rate's error at
/// the current clock); the Serial tab passes one that says nothing.
pub fn baud_picker(
    ui: &mut egui::Ui,
    id_salt: &str,
    baud: &mut u32,
    range: std::ops::RangeInclusive<u32>,
    tag: &dyn Fn(u32) -> Option<BaudTag>,
) {
    // "Custom…" with a preset still in the field is a state the value alone
    // cannot hold, so it is remembered per picker until a preset is chosen.
    let custom_id = ui.make_persistent_id((id_salt, "baud_custom"));
    let mut custom =
        !is_preset(*baud) || ui.data(|d| d.get_temp::<bool>(custom_id)).unwrap_or(false);
    ui.horizontal(|ui| {
        let shown = if custom {
            "Custom".to_owned()
        } else {
            baud.to_string()
        };
        egui::ComboBox::from_id_salt(id_salt)
            .selected_text(shown)
            .show_ui(ui, |ui| {
                for b in BAUDS {
                    let t = tag(b);
                    let mut text = egui::RichText::new(match &t {
                        Some(t) if !t.text.is_empty() => format!("{b}   {}", t.text),
                        _ => b.to_string(),
                    });
                    if let Some(t) = &t {
                        text = text.color(t.color);
                    }
                    let mut r = ui.selectable_label(!custom && *baud == b, text);
                    if let Some(t) = t {
                        r = r.on_hover_text(t.hover);
                    }
                    if r.clicked() {
                        *baud = b;
                        custom = false;
                    }
                }
                ui.separator();
                if ui
                    .selectable_label(custom, "Custom…")
                    .on_hover_text("Type any rate - the one the other end of the link runs at.")
                    .clicked()
                {
                    custom = true;
                }
            });
        if custom {
            ui.add(
                crate::panels::drag_value(ui, baud)
                    .range(range)
                    .clamp_existing_to_range(false)
                    .update_while_editing(false)
                    .speed(0.0)
                    .suffix(" baud"),
            )
            .on_hover_text("Click and type the rate; Enter applies it.");
        }
    });
    // Only the explicit choice is remembered. Storing `custom` itself would
    // latch it: a picker once shown a non-preset (a seeded 250000, say) would
    // stay on "Custom" after the value became a preset from outside.
    ui.data_mut(|d| d.insert_temp(custom_id, custom && is_preset(*baud)));
}

/// One contiguous burst of bytes in one direction.
///
/// Chunks, not a flat buffer: the whole point of Bridge mode is seeing WHO said
/// what, and a merged stream loses exactly that. Consecutive bursts in the same
/// direction are coalesced while they keep arriving, so a chatty device doesn't
/// produce a chunk per byte.
#[derive(Clone, Debug)]
pub struct LogChunk {
    pub dir: Dir,
    pub bytes: Vec<u8>,
    /// When the block's FIRST burst was read — what gets displayed.
    pub at: Instant,
    /// When its LAST burst was read. The gap rule measures from here, not from
    /// `at`: a 300 ms frame arriving in 10 ms pieces is one block, not fifteen.
    pub last: Instant,
}

/// Shared between the background reader thread and the UI.
pub struct SerialState {
    /// Raw received bytes (capped to the last [`RX_CAP`]).
    pub rx: Vec<u8>,
    /// Monotonic count of ALL bytes ever received on this port (never trimmed
    /// like `rx`) — lets incremental consumers (the plotter) know exactly how
    /// many tail bytes of `rx` are new since their last look.
    pub rx_total: u64,
    /// `true` while a reader thread is alive and the port is open.
    pub connected: bool,
    /// Last open / read / write error, shown in the UI.
    pub error: Option<String>,
    /// Bridge mode only: the relayed traffic, in order, tagged by direction.
    pub log: Vec<LogChunk>,
    /// Idle gap (ms) that ends a block. Live-tunable: the right value depends on
    /// the protocol, and you rarely know it before watching the traffic.
    pub block_gap_ms: u64,
    /// `(Instant, SystemTime)` captured at connect, so a block's monotonic
    /// timestamp can be shown as a wall clock.
    ///
    /// Anchored ONCE rather than reading the system clock per burst: that keeps
    /// the hot relay path free of a syscall, and — more importantly — makes the
    /// gaps between blocks immune to the wall clock being stepped (NTP, DST)
    /// mid-capture.
    pub epoch: Option<(Instant, std::time::SystemTime)>,
    /// egui pass of the last frame the Serial tab drew; the readers repaint
    /// only while it is recent (`terminal::drawn_recently`).
    pub drawn_pass: u64,
}

impl Default for SerialState {
    fn default() -> Self {
        Self {
            rx: Vec::new(),
            rx_total: 0,
            connected: false,
            error: None,
            log: Vec::new(),
            block_gap_ms: DEFAULT_BLOCK_GAP_MS,
            epoch: None,
            drawn_pass: 0,
        }
    }
}

impl SerialState {
    /// Append a relayed burst at time `now`, and trim past [`LOG_CAP`].
    ///
    /// A burst joins the previous block when it came from the same side AND
    /// arrived within `gap` of that block's last burst; otherwise it starts a
    /// new one. Serial has no framing at the OS level, so an idle gap is the
    /// only thing that marks a message boundary — the same rule Modbus RTU uses.
    /// Splitting purely per `read()` would tear one frame into pieces, since a
    /// read returns whatever happened to have accumulated.
    pub fn push_log(&mut self, dir: Dir, bytes: &[u8], now: Instant, gap: Duration) {
        let join = matches!(
            self.log.last(),
            Some(last) if last.dir == dir
                && now.saturating_duration_since(last.last) <= gap
                && last.bytes.len() + bytes.len() <= MAX_BLOCK
        );
        if join {
            let last = self.log.last_mut().expect("checked above");
            last.bytes.extend_from_slice(bytes);
            last.last = now;
        } else {
            self.log.push(LogChunk {
                dir,
                bytes: bytes.to_vec(),
                at: now,
                last: now,
            });
        }
        // Drop whole chunks from the front rather than splitting one: a
        // half-chunk would show a frame starting mid-byte, which is worse than
        // showing less history.
        let mut total: usize = self.log.iter().map(|c| c.bytes.len()).sum();
        while total > LOG_CAP && self.log.len() > 1 {
            total -= self.log.remove(0).bytes.len();
        }
    }
}

/// Which view fills the RX area. They all want the same space, so exactly one
/// is active — an enum instead of three booleans that could disagree.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SerialView {
    /// The raw stream, as text or coloured hex.
    #[default]
    Raw,
    /// The newest framed payload as a grid of numbers.
    Matrix,
    /// One row per protocol frame.
    Frames,
    /// Numeric lines as live curves.
    Plot,
}

impl SerialView {
    pub fn label(self) -> &'static str {
        match self {
            SerialView::Raw => "Default",
            SerialView::Matrix => "Matrix",
            SerialView::Frames => "Frames",
            SerialView::Plot => "Plot",
        }
    }
}

/// Serial monitor state owned by `AppIde`: the shared RX buffer, the write
/// handle (while connected), and the UI selections.
pub struct SerialMonitor {
    pub state: Arc<Mutex<SerialState>>,
    /// Write half of the open port (`try_clone`d from the reader's handle).
    writer: Option<Box<dyn serialport::SerialPort>>,
    /// Per-connection stop flag for the reader thread. A fresh one is made each
    /// `connect` so a quick disconnect→reconnect can't leave the old thread
    /// running (the old flag stays `true`).
    stop: Option<Arc<AtomicBool>>,
    /// Selected port name (e.g. `COM4` / `/dev/ttyUSB0`).
    pub port: String,
    /// Selected baud rate.
    pub baud: u32,
    /// Display mode: hex bytes vs. decoded text.
    pub hex: bool,
    /// Number of bytes per repeating sequence to colour / list (hex mode).
    pub seq_len: usize,
    /// Bytes shown per line in the hex view.
    pub row_bytes: usize,
    /// Width (px) of the unique-sequences legend (draggable divider).
    pub legend_w: f32,
    /// Hex byte sequences to search/highlight in the RX view (yellow / blue).
    pub search: String,
    pub search2: String,
    /// Keep the RX view pinned to the newest data.
    pub autoscroll: bool,
    /// Append `\r\n` to each sent line.
    pub append_crlf: bool,
    /// The TX input line.
    pub tx_input: String,
    /// Height (px) of the resizable send area (dragged via its handle).
    pub tx_height: f32,
    /// Pause (ms) inserted between each line when sending a multi-line block —
    /// for command sequences where the device needs time before the next one.
    pub line_delay_ms: u64,
    /// Lines still waiting to be sent (front = next), each already fully encoded
    /// (hex parsed + optional CR+LF). Drained one-per-`line_delay_ms` by
    /// [`pump_tx_queue`], so the UI never blocks while pacing a sequence.
    tx_queue: std::collections::VecDeque<Vec<u8>>,
    /// When the next queued line is due to go out.
    tx_next_at: Option<Instant>,
    /// Cached list of available ports (refreshed on demand).
    pub ports: Vec<String>,
    /// `refresh_ports` has run at least once. The Serial tab used an EMPTY
    /// `ports` as "never scanned" — and on a machine with no serial port the
    /// list stays empty, so it rescanned on every frame: a SetupAPI walk plus two
    /// `reg query /s` processes (com0com) on the UI thread, per mouse move.
    ports_scanned: bool,
    /// Port name -> what is on the other end of it, for the ports that say.
    /// Filled by the same pass as [`SerialMonitor::ports`]; see [`port_label`].
    pub port_labels: HashMap<String, String>,
    /// `false` until the baud has been seeded from the LOWEST-instance USART
    /// virtual module (done when the Serial tab first opens while idle —
    /// replaces the old toolbar Serial button's seeding). Cleared by
    /// `load_project_from_dir`: it is once per PROJECT, not once per process.
    pub baud_seeded: bool,
    /// `true` → the RX area shows the live plot instead of the text/hex view
    /// (the send area keeps working, so commands can be sent while plotting).
    pub plot_on: bool,
    /// The plotter's parsed channels + view options (see `crate::serial_plot`).
    pub plot: crate::serial_plot::PlotState,
    /// The 2D matrix view of framed payloads (see `crate::serial_matrix`).
    pub matrix: crate::serial_matrix::MatrixView,
    /// `true` → the RX area lists one row per protocol FRAME instead of the
    /// byte stream (see `crate::serial_frames`).
    pub frames_on: bool,
    /// How frames are delimited for that view.
    pub frame_spec: crate::serial_frames::FrameSpec,

    // ── Bridge (MITM) mode ───────────────────────────────────────────────────
    /// Relay the port instead of opening it directly (see
    /// [`crate::serial_bridge`]). Locked while connected — the wiring can't
    /// change under a live relay.
    pub bridge: bool,
    /// The IDE's end of the virtual pair. On Unix it is filled in by "Create
    /// pair"; on Windows the user picks one half of their com0com pair.
    pub bridge_port: String,
    /// The live pair. Held here so dropping it (disconnect / app exit) tears
    /// down the `socat` child that owns the PTYs.
    pub pair: Option<crate::serial_bridge::VirtualPair>,
    /// Show the `[hh:mm:ss.mmm] (+n ms)` prefix on each Bridge block. On by
    /// default: the one thing a relay capture is always missing is WHEN.
    pub stamps: bool,
    /// `true` → the RX area shows the drawn "how Bridge works" explainer.
    /// Its own toggle rather than a dialog: the wiring is what you consult it
    /// WHILE setting up, so it has to live where the controls are.
    pub info_on: bool,
    /// Detected com0com pairs (Windows), refreshed with the port list.
    ///
    /// CACHED on purpose: discovering them costs two `reg query` spawns, and the
    /// Bridge row is drawn every frame. Pairs only change when the user edits
    /// them in com0com's setup — which changes the port list too, so refreshing
    /// both together is exactly right.
    pub com0com_pairs: Vec<(String, String)>,
}

impl Default for SerialMonitor {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(SerialState::default())),
            writer: None,
            stop: None,
            port: String::new(),
            baud: 115_200,
            hex: false,
            seq_len: 1,
            row_bytes: 16,
            legend_w: 160.0,
            search: String::new(),
            search2: String::new(),
            autoscroll: true,
            append_crlf: false,
            tx_input: String::new(),
            tx_height: 30.0,
            line_delay_ms: 100,
            tx_queue: std::collections::VecDeque::new(),
            tx_next_at: None,
            ports: Vec::new(),
            ports_scanned: false,
            port_labels: HashMap::new(),
            baud_seeded: false,
            plot_on: false,
            plot: Default::default(),
            matrix: Default::default(),
            frames_on: false,
            frame_spec: Default::default(),
            bridge: false,
            bridge_port: String::new(),
            pair: None,
            stamps: true,
            info_on: false,
            com0com_pairs: Vec::new(),
        }
    }
}

/// What is on the other end of a port, when the OS says enough to tell.
///
/// The identity was there all along and thrown away: `available_ports` hands
/// back a `SerialPortType`, and the Flash tab already resolves the very same
/// VID:PID through [`crate::dfu::find_programmer`] - so an Espressif
/// `303a:1001` is named "Espressif built-in USB-Serial/JTAG" over there and was
/// a bare `COM7` here. It matters most on Espressif boards, which routinely
/// expose TWO ports (the chip's own USB-Serial/JTAG next to a CP210x/CH340
/// bridge) with nothing but the number to tell them apart.
///
/// `None` for a port the OS does not describe as USB - there is nothing honest
/// to say about it.
fn port_label(info: &serialport::SerialPortInfo) -> Option<String> {
    let serialport::SerialPortType::UsbPort(usb) = &info.port_type else {
        return None;
    };
    let vid_pid = format!("{:04x}:{:04x}", usb.vid, usb.pid);
    // An EXACT id in the catalogue is curated, and names the peripheral better
    // than the device names itself ("Espressif built-in USB-Serial/JTAG" against
    // the descriptor's "USB JTAG/serial debug unit"), so it wins.
    if let Some((name, _)) = crate::dfu::exact_programmer(&vid_pid) {
        return Some(name.to_owned());
    }
    // Past that, what the device says about ITSELF beats `find_programmer`'s
    // by-VID guess, which answers with the first row of that vendor - fine for
    // deciding whether a thing can be flashed, wrong when the answer is printed
    // as this port's name.
    if let Some(said) = usb.product.as_deref().or(usb.manufacturer.as_deref()) {
        return Some(said.to_owned());
    }
    // Nothing but the id left: the vendor guess beats a bare number.
    crate::dfu::find_programmer(&vid_pid).map(|(name, _)| name.to_owned())
}

impl SerialMonitor {
    /// Re-enumerate the available serial ports; pick the first one if none chosen.
    pub fn refresh_ports(&mut self) {
        self.ports_scanned = true;
        let found = serialport::available_ports().unwrap_or_default();
        self.port_labels = found
            .iter()
            .filter_map(|p| port_label(p).map(|l| (p.port_name.clone(), l)))
            .collect();
        self.ports = found.into_iter().map(|p| p.port_name).collect();
        if self.port.is_empty() {
            if let Some(first) = self.ports.first() {
                self.port = first.clone();
            }
        }
        // Same trigger, same cadence: a com0com pair appearing IS a port-list
        // change, so there is no case where one is stale and the other fresh.
        self.com0com_pairs = crate::serial_bridge::com0com_pairs(&self.ports);
    }

    /// No scan has run yet. A board plugged in later shows up on Refresh — an
    /// automatic rescan on a timer would still freeze a frame on the UI thread.
    pub fn ports_never_scanned(&self) -> bool {
        !self.ports_scanned
    }

    pub fn is_connected(&self) -> bool {
        self.state.lock().unwrap().connected
    }

    /// The active view, read off the per-view flags the renderers still use.
    pub fn view(&self) -> SerialView {
        if self.plot_on {
            SerialView::Plot
        } else if self.matrix.on {
            SerialView::Matrix
        } else if self.frames_on {
            SerialView::Frames
        } else {
            SerialView::Raw
        }
    }

    /// Switch views. Setting all three flags from one place is what keeps them
    /// from disagreeing — two "on" at once used to be a click away.
    pub fn set_view(&mut self, v: SerialView) {
        self.plot_on = v == SerialView::Plot;
        self.matrix.on = v == SerialView::Matrix;
        self.frames_on = v == SerialView::Frames;
    }

    /// Open `self.port` at `self.baud` and start the background reader.
    ///
    /// DATA ONLY: nothing here drives DTR or RTS, and nothing anywhere in `src/`
    /// does. On an Espressif board those two lines are the reset and the
    /// boot-mode select - through the two-transistor circuit on the esp32,
    /// esp32c2 and esp32s2 bridge boards, and in silicon on the six parts with
    /// the native USB-Serial/JTAG - so this tab cannot restart a chip or catch
    /// the `println!`s that happen before `main` settles. That is exactly why the
    /// Flash tab spawns `espflash monitor` instead (see [`crate::esp_monitor`]):
    /// espflash drives the lines.
    ///
    /// What the lines DO end up at is `serialport`'s per-platform default, and
    /// the two platforms differ: on Windows `dcb::init` deasserts both for the
    /// whole session, while on Linux DTR is asserted on open and the crate
    /// documents that it cannot be prevented. `required_tools.rs` already calls
    /// that a board reset, which is why it probes tty access with `access()`
    /// rather than by opening the device.
    pub fn connect(&mut self, ctx: &egui::Context) {
        if self.port.is_empty() || self.is_connected() {
            return;
        }
        // Make sure any previous reader is signalled to stop (defensive).
        if let Some(s) = self.stop.take() {
            s.store(true, Ordering::Relaxed);
        }
        {
            let mut s = self.state.lock().unwrap();
            s.error = None;
            s.connected = false;
            s.log.clear();
            // Anchor the clock for this session, exactly like the bridge: the
            // plain console timestamps its blocks off the same epoch.
            s.epoch = Some((Instant::now(), std::time::SystemTime::now()));
        }
        match serialport::new(&self.port, self.baud)
            .timeout(Duration::from_millis(100))
            .open()
        {
            Ok(port) => match port.try_clone() {
                Ok(reader) => {
                    self.writer = Some(port);
                    self.state.lock().unwrap().connected = true;
                    let stop = Arc::new(AtomicBool::new(false));
                    self.stop = Some(Arc::clone(&stop));
                    spawn_reader(reader, Arc::clone(&self.state), stop, ctx.clone());
                }
                Err(e) => self.state.lock().unwrap().error = Some(e.to_string()),
            },
            Err(e) => self.state.lock().unwrap().error = Some(e.to_string()),
        }
    }

    /// Create the virtual pair (Unix / socat only — a com0com pair already
    /// exists and is picked from the port list). Fills in `bridge_port`.
    pub fn create_pair(&mut self) {
        // Tear the old one down first, or its symlinks make socat fail.
        self.pair = None;
        match crate::serial_bridge::create_socat_pair() {
            Ok(p) => {
                self.bridge_port = p.ide_side.clone();
                self.pair = Some(p);
                self.state.lock().unwrap().error = None;
            }
            Err(e) => self.state.lock().unwrap().error = Some(e),
        }
    }

    /// Open BOTH ends and relay between them, logging each direction.
    ///
    /// `self.port` is the real device; `self.bridge_port` is the IDE's end of
    /// the virtual pair, whose mate the other application holds. Both are opened
    /// at the SAME baud: the pair is a byte pipe, but the device is not, and a
    /// mismatch here corrupts every frame in a way that looks like noise.
    ///
    /// BYTES only, which is what the UI promises and is worth reading literally:
    /// the relay never carries DTR or RTS. So Bridge can watch an application
    /// talk to a running board, but it cannot sit between `espflash` (or any
    /// other Espressif host tool) and a chip, because the download loader is
    /// entered by toggling exactly those two lines.
    pub fn connect_bridge(&mut self, ctx: &egui::Context) {
        if self.is_connected() {
            return;
        }
        if self.port.is_empty() || self.bridge_port.is_empty() {
            self.state.lock().unwrap().error =
                Some("Bridge needs both a device port and a virtual-pair port.".into());
            return;
        }
        if let Some(s) = self.stop.take() {
            s.store(true, Ordering::Relaxed);
        }
        {
            let mut s = self.state.lock().unwrap();
            s.error = None;
            s.connected = false;
            s.log.clear();
            // Anchor the clock for this capture. Re-anchored per connect, so a
            // long-running app that reconnects doesn't drift.
            s.epoch = Some((Instant::now(), std::time::SystemTime::now()));
        }

        let open = |name: &str, baud: u32| {
            serialport::new(name, baud)
                .timeout(Duration::from_millis(100))
                .open()
                .map_err(|e| format!("{name}: {e}"))
        };
        // Open the DEVICE first: it is the end that can legitimately be busy
        // (that is why the user is here), so failing before touching the pair
        // keeps the error about the thing that actually went wrong.
        let sensor = match open(&self.port, self.baud) {
            Ok(p) => p,
            Err(e) => {
                self.state.lock().unwrap().error = Some(e);
                return;
            }
        };
        let app = match open(&self.bridge_port, self.baud) {
            Ok(p) => p,
            Err(e) => {
                self.state.lock().unwrap().error = Some(e);
                return;
            }
        };
        // Each direction needs a reader on one port and a writer on the other,
        // so both ports are cloned: four handles, two threads.
        let (Ok(sensor_w), Ok(app_w)) = (sensor.try_clone(), app.try_clone()) else {
            self.state.lock().unwrap().error =
                Some("could not duplicate the port handles for relaying".into());
            return;
        };

        self.state.lock().unwrap().connected = true;
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = Some(Arc::clone(&stop));
        // device → app
        spawn_bridge_reader(
            sensor,
            app_w,
            Dir::SensorToApp,
            Arc::clone(&self.state),
            Arc::clone(&stop),
            ctx.clone(),
        );
        // app → device
        spawn_bridge_reader(
            app,
            sensor_w,
            Dir::AppToSensor,
            Arc::clone(&self.state),
            stop,
            ctx.clone(),
        );
        // In bridge mode the IDE is a relay, not a participant: it must not
        // inject bytes of its own, so there is no writer handle for `send`.
        self.writer = None;
    }

    /// Stop the reader thread and release the port.
    pub fn disconnect(&mut self) {
        if let Some(s) = self.stop.take() {
            s.store(true, Ordering::Relaxed);
        }
        self.state.lock().unwrap().connected = false;
        self.writer = None;
        // Drop any not-yet-sent queued lines (the port is gone).
        self.tx_queue.clear();
        self.tx_next_at = None;
        // Drops the socat child with it (see `VirtualPair`). Deliberate: leaving
        // PTYs behind after a disconnect would collide with the next Create pair.
        self.pair = None;
    }

    /// Send raw bytes on the open port (no-op when disconnected).
    ///
    /// The write is logged as its own timestamped block, so the console can pair
    /// a command with the reply that follows and show the gap between them —
    /// which is the whole reason to look at a serial log during bring-up.
    /// Timestamped AFTER the write returns: that is the moment the bytes are
    /// with the driver, and it is the honest end of "when did I send it".
    pub fn send(&mut self, bytes: &[u8]) {
        if let Some(w) = self.writer.as_mut() {
            if let Err(e) = w.write_all(bytes) {
                self.state.lock().unwrap().error = Some(e.to_string());
                return;
            }
            let mut s = self.state.lock().unwrap();
            let gap = Duration::from_millis(s.block_gap_ms);
            s.push_log(Dir::AppToSensor, bytes, Instant::now(), gap);
        }
    }

    /// Queue `lines` to be sent one at a time, `line_delay_ms` apart (the first
    /// goes out on the next [`pump_tx_queue`]). Replaces any pending queue.
    pub fn queue_lines(&mut self, lines: Vec<Vec<u8>>) {
        self.tx_queue = lines.into();
        self.tx_next_at = if self.tx_queue.is_empty() {
            None
        } else {
            Some(Instant::now()) // first line is due immediately
        };
    }

    /// Send every queued line whose delay has elapsed (all of them at once when
    /// `line_delay_ms == 0`, so a zero gap is truly back-to-back). Returns the
    /// time until the next not-yet-due line (so the caller can
    /// `request_repaint_after` it), or `None` when the queue is empty. Call once
    /// per frame.
    pub fn pump_tx_queue(&mut self) -> Option<Duration> {
        while self.tx_next_at.is_some_and(|at| Instant::now() >= at) {
            if let Some(bytes) = self.tx_queue.pop_front() {
                self.send(&bytes);
            }
            self.tx_next_at = if self.tx_queue.is_empty() {
                None
            } else {
                Some(Instant::now() + Duration::from_millis(self.line_delay_ms))
            };
        }
        self.tx_next_at
            .map(|at| at.saturating_duration_since(Instant::now()))
    }

    /// Idle gap (ms) that ends a Bridge block. Lives in the shared state because
    /// the relay thread reads it on every burst.
    pub fn block_gap_ms(&self) -> u64 {
        self.state.lock().unwrap().block_gap_ms
    }

    pub fn set_block_gap_ms(&mut self, ms: u64) {
        self.state.lock().unwrap().block_gap_ms = ms;
    }

    pub fn clear_rx(&mut self) {
        let mut s = self.state.lock().unwrap();
        s.rx.clear();
        // The timed view reads the SAME session from `log` — leaving it behind
        // would make Clear look like it did nothing there.
        s.log.clear();
    }
}

/// Background loop: read bytes into the shared buffer until asked to stop or the
/// port errors. The short read timeout lets it notice `stop` promptly. Repaints
/// are throttled (~30 fps) so a continuously-streaming device doesn't pin the UI
/// at max frame-rate and starve the rest of the app.
fn spawn_reader(
    mut port: Box<dyn serialport::SerialPort>,
    state: Arc<Mutex<SerialState>>,
    stop: Arc<AtomicBool>,
    ctx: egui::Context,
) {
    const REPAINT_EVERY: Duration = Duration::from_millis(33);
    std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        let mut last_repaint = Instant::now() - REPAINT_EVERY;
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            match port.read(&mut buf) {
                Ok(0) => {
                    // No data but not a timeout — avoid a busy-spin.
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(n) => {
                    let mut s = state.lock().unwrap();
                    s.rx.extend_from_slice(&buf[..n]);
                    s.rx_total += n as u64;
                    if s.rx.len() > RX_CAP {
                        let excess = s.rx.len() - RX_CAP;
                        s.rx.drain(..excess);
                    }
                    // Also as a timestamped block, so the plain console can show
                    // WHEN each burst arrived — the number that answers "how
                    // long after my command did it reply?". The raw `rx` stream
                    // stays untouched for the text view, the hex view and the
                    // plotter.
                    let gap = Duration::from_millis(s.block_gap_ms);
                    s.push_log(Dir::SensorToApp, &buf[..n], Instant::now(), gap);
                    let drawn_pass = s.drawn_pass;
                    drop(s);
                    // Coalesce repaints: at most one ~every 33 ms while streaming,
                    // and none while the Serial tab is not on screen or the
                    // window is minimized. Errors below still repaint
                    // unconditionally.
                    if !crate::terminal::stream_wants_repaint(&ctx, drawn_pass) {
                        // Hidden: the bytes wait in `rx` / `log`.
                    } else if last_repaint.elapsed() >= REPAINT_EVERY {
                        ctx.request_repaint();
                        last_repaint = Instant::now();
                    } else {
                        ctx.request_repaint_after(REPAINT_EVERY);
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => {
                    let mut s = state.lock().unwrap();
                    s.error = Some(e.to_string());
                    s.connected = false;
                    drop(s);
                    ctx.request_repaint();
                    break;
                }
            }
        }
    });
}

/// One direction of the bridge: read `from` to `to`, logging every burst.
///
/// Forwarding comes FIRST and the log second — the two applications are talking
/// to each other in real time, and a stall while the UI mutex is contended would
/// show up as a protocol timeout on the wire. Logging is the side effect here,
/// not the job.
fn spawn_bridge_reader(
    mut from: Box<dyn serialport::SerialPort>,
    mut to: Box<dyn serialport::SerialPort>,
    dir: Dir,
    state: Arc<Mutex<SerialState>>,
    stop: Arc<AtomicBool>,
    ctx: egui::Context,
) {
    const REPAINT_EVERY: Duration = Duration::from_millis(33);
    std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        let mut last_repaint = Instant::now() - REPAINT_EVERY;
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            match from.read(&mut buf) {
                Ok(0) => std::thread::sleep(Duration::from_millis(5)),
                Ok(n) => {
                    // Timestamp BEFORE forwarding: the read is what we can date,
                    // and the write that follows is our own latency, not the
                    // other side's.
                    let now = Instant::now();
                    let relayed = to.write_all(&buf[..n]).and_then(|()| to.flush());
                    let drawn_pass = {
                        let mut s = state.lock().unwrap();
                        let gap = Duration::from_millis(s.block_gap_ms);
                        s.push_log(dir, &buf[..n], now, gap);
                        if let Err(e) = relayed {
                            // Report but keep relaying the other way: half a
                            // bridge still shows what the live side is saying.
                            s.error = Some(format!("relay write failed: {e}"));
                        }
                        s.drawn_pass
                    };
                    if !crate::terminal::stream_wants_repaint(&ctx, drawn_pass) {
                        // Hidden: the relay goes on, only the redraw waits.
                    } else if last_repaint.elapsed() >= REPAINT_EVERY {
                        ctx.request_repaint();
                        last_repaint = Instant::now();
                    } else {
                        ctx.request_repaint_after(REPAINT_EVERY);
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => {
                    let mut s = state.lock().unwrap();
                    s.error = Some(e.to_string());
                    s.connected = false;
                    drop(s);
                    ctx.request_repaint();
                    break;
                }
            }
        }
    });
}

/// Colour of traffic heading TO the device (the other app's requests).
pub const DIR_APP: egui::Color32 = egui::Color32::from_rgb(235, 180, 90);
/// Colour of traffic coming FROM the device (its replies).
pub const DIR_SENSOR: egui::Color32 = egui::Color32::from_rgb(110, 200, 225);

/// Every start index at which `pat` occurs in `hay`. Empty pattern → no hits,
/// so an empty Find field never "matches everything".
pub(crate) fn match_positions(hay: &[u8], pat: &[u8]) -> Vec<usize> {
    if pat.is_empty() || pat.len() > hay.len() {
        return Vec::new();
    }
    (0..=hay.len() - pat.len())
        .filter(|&i| &hay[i..i + pat.len()] == pat)
        .collect()
}

/// Does this burst contain any of the markers? The filter predicate.
fn chunk_matches(bytes: &[u8], a: &[u8], b: &[u8]) -> bool {
    !match_positions(bytes, a).is_empty() || !match_positions(bytes, b).is_empty()
}

/// Render the bridge log: one line per burst, prefixed `>>` for app→device and
/// `<<` for device→app, in hex or lossy text.
///
/// Two colours and two arrows rather than one merged stream — reading a MITM
/// capture is entirely about attributing each byte to a side. Only the tail is
/// laid out: a long session's log is far bigger than any screen.
///
/// `find_a` / `find_b` do two jobs at once, which is what makes them useful on a
/// relay: bursts containing NEITHER marker are dropped (a device that polls
/// every 50 ms buries the exchange you care about), and inside the ones kept,
/// the markers are painted yellow / cyan so a frame's edges are findable at a
/// glance. Both empty = no filtering, everything shown.
/// Colour of the `[hh:mm:ss.mmm] (+n ms)` prefix — dim, so it frames the data
/// without competing with the direction colours.
pub const STAMP: egui::Color32 = egui::Color32::from_rgb(130, 138, 152);

/// The timestamp prefix for one block: wall clock, plus the gap since the
/// previous block.
///
/// The DELTA is the useful number when reverse-engineering a protocol — "the
/// reply came 12 ms later" tells you more than the absolute time — so it is
/// shown alongside, not instead.
///
/// `epoch` anchors the monotonic `Instant` to a wall clock; without it only the
/// delta can be shown, which is still worth having.
fn stamp_prefix(
    at: Instant,
    prev: Option<Instant>,
    epoch: Option<(Instant, std::time::SystemTime)>,
) -> String {
    let clock = match epoch {
        Some((i0, t0)) => crate::activity::fmt_clock(t0 + at.saturating_duration_since(i0)),
        None => "--:--:--.---".to_string(),
    };
    match prev {
        Some(p) => format!(
            "[{clock}] (+{} ms) ",
            at.saturating_duration_since(p).as_millis()
        ),
        None => format!("[{clock}]         "),
    }
}

/// `filter`: what a Find field DOES to non-matching blocks.
///
/// The Bridge filters them out — you are hunting one frame in a relayed
/// conversation you don't control. The plain console highlights instead: there
/// you are reading your OWN exchange, and dropping every block but the hit
/// throws away the reply you were timing (and shows an empty pane the moment a
/// pattern matches nothing yet).
#[allow(clippy::too_many_arguments)]
pub fn bridge_log_job(
    log: &[LogChunk],
    hex: bool,
    font_size: f32,
    find_a: &[u8],
    find_b: &[u8],
    stamps: bool,
    epoch: Option<(Instant, std::time::SystemTime)>,
    filter: bool,
) -> egui::text::LayoutJob {
    const MAX_CHUNKS: usize = 400;
    let filtering = filter && (!find_a.is_empty() || !find_b.is_empty());
    let font = egui::FontId::monospace(font_size);
    let mut job = egui::text::LayoutJob::default();

    // Filter FIRST, then take the tail: taking the tail first would leave a
    // screen of nothing whenever the matches are older than the last 400 bursts.
    let kept: Vec<&LogChunk> = log
        .iter()
        .filter(|c| !filtering || chunk_matches(&c.bytes, find_a, find_b))
        .collect();
    let start = kept.len().saturating_sub(MAX_CHUNKS);

    let mut prev: Option<Instant> = None;
    for chunk in &kept[start..] {
        let (arrow, color) = match chunk.dir {
            Dir::AppToSensor => (">>", DIR_APP),
            Dir::SensorToApp => ("<<", DIR_SENSOR),
        };
        if stamps {
            // Delta against the previous SHOWN block: with a filter on, the gap
            // to a burst that was hidden would be a number about nothing.
            job.append(
                &stamp_prefix(chunk.at, prev, epoch),
                0.0,
                egui::TextFormat::simple(font.clone(), STAMP),
            );
            prev = Some(chunk.at);
        }
        job.append(
            &format!("{arrow} "),
            0.0,
            egui::TextFormat::simple(font.clone(), color),
        );

        // One colour per byte: the burst's direction, overridden where a marker
        // sits. Emitted as RUNS of equal colour so a 1 kB frame is a handful of
        // sections, not a thousand.
        let n = chunk.bytes.len();
        let mut colors = vec![color; n];
        for (pat, hit) in [(find_a, SEARCH_HIT), (find_b, SEARCH_HIT2)] {
            for i in match_positions(&chunk.bytes, pat) {
                for c in colors.iter_mut().skip(i).take(pat.len()) {
                    *c = hit;
                }
            }
        }
        let mut i = 0;
        while i < n {
            let mut j = i + 1;
            while j < n && colors[j] == colors[i] {
                j += 1;
            }
            let run = &chunk.bytes[i..j];
            let text = if hex {
                run.iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(" ")
                    + " "
            } else {
                // Trailing newlines would double-space the view: the arrow
                // prefix already puts each burst on its own line.
                String::from_utf8_lossy(run)
                    .trim_end_matches(['\r', '\n'])
                    .to_string()
            };
            job.append(
                &text,
                0.0,
                egui::TextFormat::simple(font.clone(), colors[i]),
            );
            i = j;
        }
        job.append("\n", 0.0, egui::TextFormat::simple(font.clone(), color));
    }
    job
}

/// Decoded-text tail of `rx` (lossy UTF-8). Only the last chunk is returned so
/// layout stays cheap. (Hex mode uses [`hex_layout_job`] for coloured output.)
pub fn render_rx_text(rx: &[u8]) -> String {
    let start = rx.len().saturating_sub(16 * 1024);
    String::from_utf8_lossy(&rx[start..]).into_owned()
}

/// A deterministic, vivid colour for a single byte — same byte → same colour,
/// nearby values clearly different (golden-ratio hue spread).
pub fn byte_color(b: u8) -> egui::Color32 {
    let hue = (b as f32 * 0.618_034).fract();
    egui::Color32::from(egui::ecolor::Hsva::new(hue, 0.6, 0.95, 1.0))
}

/// A deterministic colour for a byte *sequence* — same sequence → same colour.
/// (Single byte falls back to [`byte_color`]; longer sequences use an FNV-1a
/// hash so repeated multi-byte frames are visually linked.)
pub fn seq_color(seq: &[u8]) -> egui::Color32 {
    if let [b] = seq {
        return byte_color(*b);
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in seq {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let hue = (h % 9973) as f32 / 9973.0;
    egui::Color32::from(egui::ecolor::Hsva::new(hue, 0.6, 0.95, 1.0))
}

/// Distinct `seq_len`-byte sequences in `bytes` with their counts, most-frequent
/// first — the "repeating sequences as a set of unique values" legend. Sequences
/// are aligned from offset 0; the trailing partial chunk is ignored.
pub fn seq_counts(bytes: &[u8], seq_len: usize) -> Vec<(Vec<u8>, u32)> {
    let seq_len = seq_len.max(1);
    let mut map: HashMap<Vec<u8>, u32> = HashMap::new();
    for chunk in bytes.chunks_exact(seq_len) {
        *map.entry(chunk.to_vec()).or_insert(0) += 1;
    }
    let mut out: Vec<(Vec<u8>, u32)> = map.into_iter().collect();
    // Most-repeated first; ties by sequence bytes.
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// Coloured hex `LayoutJob` for the RX view: bytes are grouped into aligned
/// `seq_len`-byte sequences (from offset 0) and every byte of a group is
/// coloured by [`seq_color`] of that group, so repeated sequences share a
/// colour. Only the tail is built so layout stays cheap.
pub fn hex_layout_job(
    bytes: &[u8],
    fontsize: f32,
    seq_len: usize,
    row_bytes: usize,
) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let seq_len = seq_len.max(1);
    let row_bytes = row_bytes.max(1);
    let font = egui::FontId::monospace(fontsize);
    let mut job = LayoutJob::default();
    let n = bytes.len();
    let start = n.saturating_sub(4 * 1024);
    let mut cur_cs = usize::MAX;
    let mut cur_color = egui::Color32::GRAY;
    for i in start..n {
        // Aligned chunk (from offset 0) containing byte i.
        let cs = (i / seq_len) * seq_len;
        if cs != cur_cs {
            let ce = (cs + seq_len).min(n);
            cur_color = seq_color(&bytes[cs..ce]);
            cur_cs = cs;
        }
        job.append(
            &format!("{:02X} ", bytes[i]),
            0.0,
            TextFormat::simple(font.clone(), cur_color),
        );
        if (i + 1) % row_bytes == 0 {
            job.append(
                "\n",
                0.0,
                TextFormat::simple(font.clone(), egui::Color32::GRAY),
            );
        }
    }
    job
}

/// Colours of the two searched sequences (yellow / blue) and the rest (grey).
pub const SEARCH_HIT: egui::Color32 = egui::Color32::from_rgb(255, 230, 0);
pub const SEARCH_HIT2: egui::Color32 = egui::Color32::from_rgb(0, 200, 250);
pub const SEARCH_MISS: egui::Color32 = egui::Color32::from_rgb(150, 150, 150);

/// Parse a user-typed hex search string into bytes: hex digits are kept (spaces
/// / other chars ignored) and grouped into pairs; a trailing lone digit is
/// dropped. `"0D 0A"` / `"0d0a"` → `[0x0D, 0x0A]`.
pub fn parse_hex_search(s: &str) -> Vec<u8> {
    let digits: Vec<u8> = s.bytes().filter(|b| b.is_ascii_hexdigit()).collect();
    digits
        .chunks_exact(2)
        .filter_map(|p| {
            let hi = (p[0] as char).to_digit(16)?;
            let lo = (p[1] as char).to_digit(16)?;
            Some((hi * 16 + lo) as u8)
        })
        .collect()
}

/// Payload ranges `[start, end)` of every complete `a` … `b` frame — the
/// bytes BETWEEN the markers, **both excluded**:
/// `FD FC FB FA | 01 02 03 04 05 | 04 03 02 01` → one range of 5 bytes.
///
/// Scanning is left to right: each `a` opens a frame, the FIRST `b` at or after
/// it closes one, and the next scan resumes past that `b` (frames never nest).
/// A second `a` seen before the closing `b` RESTARTS the frame there — a
/// truncated frame must not bleed into the next one. `b` is matched before
/// `a`, so identical patterns delimit consecutive markers. A trailing `a`
/// with no `b` yet contributes nothing (the frame is still incoming). Empty
/// patterns yield no frames. Feeds both the "Between" byte counter and the
/// Matrix view (which decodes the LAST complete payload).
pub fn frame_ranges(bytes: &[u8], a: &[u8], b: &[u8]) -> Vec<(usize, usize)> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + a.len() <= bytes.len() {
        if &bytes[i..i + a.len()] != a {
            i += 1;
            continue;
        }
        // Frame opened — walk forward to its closing marker.
        let mut start = i + a.len();
        let mut j = start;
        let mut closed = None;
        while j < bytes.len() {
            if bytes[j..].starts_with(b) {
                closed = Some(j);
                break;
            }
            if bytes[j..].starts_with(a) {
                // A fresh header before the tail: the previous frame was cut
                // short — measure from this one instead.
                start = j + a.len();
                j = start;
                continue;
            }
            j += 1;
        }
        match closed {
            Some(end) => {
                out.push((start, end));
                i = end + b.len();
            }
            // No closing marker (yet) — the rest of the buffer is a partial frame.
            None => break,
        }
    }
    out
}

/// Byte counts BETWEEN each `a` … `b` pair (the payload length of every
/// frame) — [`frame_ranges`] reduced to lengths, for the "Between" counter.
pub fn gap_counts(bytes: &[u8], a: &[u8], b: &[u8]) -> Vec<usize> {
    frame_ranges(bytes, a, b)
        .into_iter()
        .map(|(s, e)| e - s)
        .collect()
}

/// Hex `LayoutJob` in *search* mode: bytes belonging to an occurrence of any
/// `(pattern, colour)` are coloured with that colour, everything else grey
/// ([`SEARCH_MISS`]). Later patterns win on overlap. Tail only; overlapping
/// matches of a pattern are all highlighted.
pub fn hex_search_job(
    bytes: &[u8],
    fontsize: f32,
    patterns: &[(&[u8], egui::Color32)],
    row_bytes: usize,
) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let row_bytes = row_bytes.max(1);
    let font = egui::FontId::monospace(fontsize);
    let mut job = LayoutJob::default();
    let n = bytes.len();
    let start = n.saturating_sub(4 * 1024);
    let tail = &bytes[start..];
    let m = tail.len();

    let mut colors = vec![SEARCH_MISS; m];
    for (pattern, col) in patterns {
        let plen = pattern.len();
        if plen == 0 || plen > m {
            continue;
        }
        let mut i = 0;
        while i + plen <= m {
            if &tail[i..i + plen] == *pattern {
                for c in colors.iter_mut().skip(i).take(plen) {
                    *c = *col;
                }
            }
            i += 1;
        }
    }

    for (i, &b) in tail.iter().enumerate() {
        job.append(
            &format!("{b:02X} "),
            0.0,
            TextFormat::simple(font.clone(), colors[i]),
        );
        if (start + i + 1) % row_bytes == 0 {
            job.append("\n", 0.0, TextFormat::simple(font.clone(), SEARCH_MISS));
        }
    }
    job
}

/// Text-view `LayoutJob`: whole lines that START (case-insensitively, after
/// leading whitespace) with `needle` are painted [`SEARCH_HIT`] yellow; every
/// other line keeps `default_color`. Used by Find-1 in the plain-text serial
/// view. `needle` is assumed non-empty (the caller renders plain text when it
/// is empty).
pub fn text_search_job(
    text: &str,
    needle: &str,
    fontsize: f32,
    default_color: egui::Color32,
) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let font = egui::FontId::monospace(fontsize);
    let needle_lc = needle.to_lowercase();
    let mut job = LayoutJob::default();
    // `split_inclusive` keeps each line's trailing '\n', so the rendered text
    // reproduces the input exactly.
    for line in text.split_inclusive('\n') {
        let hit = line.trim_start().to_lowercase().starts_with(&needle_lc);
        let color = if hit { SEARCH_HIT } else { default_color };
        job.append(line, 0.0, TextFormat::simple(font.clone(), color));
    }
    job
}

#[cfg(test)]
mod bridge_tests {
    use super::*;

    const GAP: Duration = Duration::from_millis(20);

    /// A controllable clock. `Instant` can only be made by `now()`, so tests
    /// anchor once and offset from it — which is exactly how the relay treats
    /// time anyway.
    struct Clock(Instant);
    impl Clock {
        fn new() -> Self {
            Self(Instant::now())
        }
        fn at(&self, ms: u64) -> Instant {
            self.0 + Duration::from_millis(ms)
        }
    }

    /// Push a burst at `ms` on the test clock, with the default gap.
    fn push(s: &mut SerialState, c: &Clock, dir: Dir, b: &[u8], ms: u64) {
        s.push_log(dir, b, c.at(ms), GAP);
    }

    fn dirs(s: &SerialState) -> Vec<(Dir, Vec<u8>)> {
        s.log.iter().map(|c| (c.dir, c.bytes.clone())).collect()
    }

    /// Find in the PLAIN console highlights but never hides: dropping the
    /// non-matching blocks would take away the reply whose latency is being
    /// read, and would blank the pane while a pattern matches nothing yet.
    #[test]
    fn plain_console_find_highlights_without_filtering() {
        let c = Clock::new();
        let mut s = SerialState::default();
        push(&mut s, &c, Dir::AppToSensor, b"cmd", 0);
        push(&mut s, &c, Dir::SensorToApp, b"reply", 30);

        // filter = false: everything stays, whether the pattern hits or misses.
        let hit = bridge_log_job(&s.log, false, 12.0, b"reply", b"", false, None, false).text;
        assert!(hit.contains("cmd") && hit.contains("reply"), "{hit}");
        let miss = bridge_log_job(&s.log, false, 12.0, b"zzz", b"", false, None, false).text;
        assert!(miss.contains("cmd") && miss.contains("reply"), "{miss}");
        // …and the match is still coloured.
        let job = bridge_log_job(&s.log, false, 12.0, b"reply", b"", false, None, false);
        let colors: Vec<egui::Color32> = job.sections.iter().map(|x| x.format.color).collect();
        assert!(colors.contains(&SEARCH_HIT), "match not highlighted");

        // filter = true (Bridge) keeps its behaviour: a miss hides the block.
        let bridged = bridge_log_job(&s.log, false, 12.0, b"zzz", b"", false, None, true).text;
        assert!(!bridged.contains("cmd"), "{bridged}");
    }

    /// Rendered text with no timestamps — for the tests that are about content.
    fn text_of(s: &SerialState, hex: bool, a: &[u8], b: &[u8]) -> String {
        bridge_log_job(&s.log, hex, 12.0, a, b, false, None, true).text
    }

    /// Bursts from the same side, arriving CLOSE TOGETHER, merge; a change of
    /// direction starts a new block. Without the merge a chatty device produces
    /// one block per read and the view becomes a column of one-byte arrows.
    #[test]
    fn same_direction_bursts_coalesce() {
        let c = Clock::new();
        let mut s = SerialState::default();
        push(&mut s, &c, Dir::AppToSensor, b"AT", 0);
        push(&mut s, &c, Dir::AppToSensor, b"+RST\r\n", 5);
        push(&mut s, &c, Dir::SensorToApp, b"OK", 7);
        push(&mut s, &c, Dir::AppToSensor, b"X", 9);
        assert_eq!(
            dirs(&s),
            vec![
                (Dir::AppToSensor, b"AT+RST\r\n".to_vec()),
                (Dir::SensorToApp, b"OK".to_vec()),
                (Dir::AppToSensor, b"X".to_vec()),
            ]
        );
    }

    /// The whole point of the gap rule: silence ends a block even when the
    /// direction has not changed. Two polls of the same device are two
    /// messages, not one long one.
    #[test]
    fn a_silent_gap_starts_a_new_block() {
        let c = Clock::new();
        let mut s = SerialState::default();
        push(&mut s, &c, Dir::SensorToApp, b"first", 0);
        push(&mut s, &c, Dir::SensorToApp, b"second", 500);
        assert_eq!(s.log.len(), 2, "{:?}", dirs(&s));
        assert_eq!(s.log[0].bytes, b"first");
        assert_eq!(s.log[1].bytes, b"second");
    }

    /// The gap is measured from the block's LAST burst, not its first — a frame
    /// dribbling in over 300 ms in 10 ms pieces is one block, not thirty.
    #[test]
    fn the_gap_is_measured_from_the_last_burst() {
        let c = Clock::new();
        let mut s = SerialState::default();
        for i in 0..30 {
            push(&mut s, &c, Dir::SensorToApp, b"x", i * 10);
        }
        assert_eq!(s.log.len(), 1, "a slow frame was torn apart");
        assert_eq!(s.log[0].bytes.len(), 30);
        // `at` stays the first burst; `last` follows the newest.
        assert_eq!(s.log[0].at, c.at(0));
        assert_eq!(s.log[0].last, c.at(290));
    }

    /// Exactly at the threshold still joins — the boundary is "longer than".
    #[test]
    fn the_threshold_is_inclusive() {
        let c = Clock::new();
        let mut s = SerialState::default();
        push(&mut s, &c, Dir::AppToSensor, b"a", 0);
        push(&mut s, &c, Dir::AppToSensor, b"b", 20);
        assert_eq!(s.log.len(), 1);
        push(&mut s, &c, Dir::AppToSensor, b"c", 41);
        assert_eq!(s.log.len(), 2);
    }

    /// Trimming drops WHOLE blocks from the front. Splitting one would leave a
    /// frame starting mid-byte, which reads as corruption rather than as history
    /// that scrolled away.
    #[test]
    fn the_log_is_capped_by_dropping_whole_chunks() {
        let c = Clock::new();
        let mut s = SerialState::default();
        for i in 0..40u64 {
            let dir = if i % 2 == 0 {
                Dir::AppToSensor
            } else {
                Dir::SensorToApp
            };
            push(&mut s, &c, dir, &vec![i as u8; LOG_CAP / 8], i);
        }
        let total: usize = s.log.iter().map(|c| c.bytes.len()).sum();
        assert!(total <= LOG_CAP, "log not trimmed: {total}");
        assert!(s.log.iter().all(|c| c.bytes.len() == LOG_CAP / 8));
        assert_eq!(s.log.last().unwrap().bytes[0], 39);
    }

    /// A device that never pauses past the gap no longer builds one endless
    /// block: joining stops at `MAX_BLOCK`, and the whole-block trim then keeps
    /// the log under its cap.
    #[test]
    fn a_stream_without_pauses_is_still_capped() {
        let c = Clock::new();
        let mut s = SerialState::default();
        for i in 0..2_000u64 {
            push(&mut s, &c, Dir::SensorToApp, &[0x55; 100], i);
        }
        assert!(s.log.len() > 1, "one block took the whole stream");
        assert!(s.log.iter().all(|b| b.bytes.len() <= MAX_BLOCK));
        let total: usize = s.log.iter().map(|b| b.bytes.len()).sum();
        assert!(total <= LOG_CAP, "log not trimmed: {total}");
    }

    /// The time view exists to show a command next to its reply. A reply that
    /// fits in one block stays one block after the command, so both lines — and
    /// the delay between them — are laid out.
    #[test]
    fn a_command_and_its_reply_stay_side_by_side() {
        let c = Clock::new();
        let mut s = SerialState::default();
        push(&mut s, &c, Dir::AppToSensor, b"GET\r\n", 0);
        for i in 0..50u64 {
            push(&mut s, &c, Dir::SensorToApp, &[0x41; 100], 5 + i * 5);
        }
        assert_eq!(s.log.len(), 2, "one command, one reply block");
        let hex = text_of(&s, true, b"", b"");
        assert!(hex.contains(">>"), "{hex}");
        assert!(hex.contains("<<"), "{hex}");
    }

    /// A single block larger than the cap must not be dropped to nothing —
    /// otherwise one big frame would clear the view instead of showing.
    #[test]
    fn one_oversized_chunk_survives() {
        let c = Clock::new();
        let mut s = SerialState::default();
        push(&mut s, &c, Dir::SensorToApp, &vec![7u8; LOG_CAP * 2], 0);
        assert_eq!(s.log.len(), 1);
        assert_eq!(s.log[0].bytes.len(), LOG_CAP * 2);
    }

    #[test]
    fn the_log_view_attributes_every_burst_to_a_side() {
        let c = Clock::new();
        let mut s = SerialState::default();
        push(&mut s, &c, Dir::AppToSensor, b"ping", 0);
        push(&mut s, &c, Dir::SensorToApp, b"pong", 1);
        let text = text_of(&s, false, b"", b"");
        assert!(text.contains(">> ping"), "{text}");
        assert!(text.contains("<< pong"), "{text}");
        let hex = text_of(&s, true, b"", b"");
        assert!(hex.contains(">> 70 69 6E 67"), "{hex}");
    }

    /// An EMPTY Find field must never act as "matches everything" — that would
    /// silently filter the log the moment one of the two fields is typed in.
    #[test]
    fn empty_markers_do_not_filter() {
        let c = Clock::new();
        let mut s = SerialState::default();
        push(&mut s, &c, Dir::AppToSensor, b"alpha", 0);
        push(&mut s, &c, Dir::SensorToApp, b"beta", 1);
        let all = text_of(&s, false, b"", b"");
        assert!(all.contains("alpha") && all.contains("beta"), "{all}");
    }

    /// The point of the filter on a relay: a device that polls constantly buries
    /// the exchange you are looking for.
    #[test]
    fn bursts_without_a_marker_are_dropped() {
        let c = Clock::new();
        let mut s = SerialState::default();
        push(&mut s, &c, Dir::AppToSensor, b"noise-1", 0);
        push(&mut s, &c, Dir::SensorToApp, b"AT+RST wanted", 1);
        push(&mut s, &c, Dir::AppToSensor, b"noise-2", 2);
        let t = text_of(&s, false, b"AT+", b"");
        assert!(t.contains("wanted"), "{t}");
        assert!(!t.contains("noise"), "kept an unmatched burst:\n{t}");
        let t2 = text_of(&s, false, b"", b"noise-2");
        assert!(t2.contains("noise-2") && !t2.contains("noise-1"), "{t2}");
    }

    /// Filtering happens BEFORE the tail cut, or a match older than the last few
    /// hundred bursts would leave the view blank.
    #[test]
    fn an_old_match_survives_the_tail_cut() {
        let c = Clock::new();
        let mut s = SerialState::default();
        push(&mut s, &c, Dir::AppToSensor, b"NEEDLE here", 0);
        for i in 0..900u64 {
            let dir = if i % 2 == 0 {
                Dir::SensorToApp
            } else {
                Dir::AppToSensor
            };
            push(&mut s, &c, dir, b"filler", i + 1);
        }
        let t = text_of(&s, false, b"NEEDLE", b"");
        assert!(t.contains("NEEDLE"), "old match dropped by the tail cut");
    }

    /// Marker bytes are coloured differently from the rest of their block, so a
    /// frame's edges are visible inside a long payload.
    #[test]
    fn markers_are_highlighted_inside_a_kept_burst() {
        let c = Clock::new();
        let mut s = SerialState::default();
        push(&mut s, &c, Dir::SensorToApp, &[0xAA, 0x01, 0x02, 0x55], 0);
        let job = bridge_log_job(&s.log, true, 12.0, &[0xAA], &[0x55], false, None, true);
        let colors: Vec<egui::Color32> = job.sections.iter().map(|x| x.format.color).collect();
        assert!(colors.contains(&SEARCH_HIT), "start marker not highlighted");
        assert!(colors.contains(&SEARCH_HIT2), "end marker not highlighted");
        assert!(
            colors.contains(&DIR_SENSOR),
            "payload lost its direction colour"
        );
    }

    /// The delta is the number you actually read when timing a protocol.
    #[test]
    fn stamps_show_the_gap_between_blocks() {
        let c = Clock::new();
        let mut s = SerialState::default();
        push(&mut s, &c, Dir::AppToSensor, b"cmd", 0);
        push(&mut s, &c, Dir::SensorToApp, b"reply", 120);
        let t = bridge_log_job(&s.log, false, 12.0, b"", b"", true, None, true).text;
        assert!(t.contains("(+120 ms)"), "{t}");
        // The first block has nothing to measure against, so no delta at all.
        assert_eq!(t.matches("(+").count(), 1, "{t}");
    }

    /// With a filter on, the delta must be against the previous SHOWN block — a
    /// gap to a burst the user cannot see is a number about nothing.
    #[test]
    fn the_delta_skips_filtered_out_blocks() {
        let c = Clock::new();
        let mut s = SerialState::default();
        push(&mut s, &c, Dir::AppToSensor, b"KEEP one", 0);
        push(&mut s, &c, Dir::SensorToApp, b"hidden", 30);
        push(&mut s, &c, Dir::AppToSensor, b"KEEP two", 100);
        let t = bridge_log_job(&s.log, false, 12.0, b"KEEP", b"", true, None, true).text;
        assert!(
            t.contains("(+100 ms)"),
            "delta should span the hidden block:\n{t}"
        );
    }

    /// Without an epoch only the delta is knowable; the clock column says so
    /// rather than inventing a time.
    #[test]
    fn a_missing_epoch_still_shows_deltas() {
        let c = Clock::new();
        let mut s = SerialState::default();
        push(&mut s, &c, Dir::AppToSensor, b"a", 0);
        push(&mut s, &c, Dir::SensorToApp, b"b", 7);
        let t = bridge_log_job(&s.log, false, 12.0, b"", b"", true, None, true).text;
        assert!(t.contains("--:--:--.---"), "{t}");
        assert!(t.contains("(+7 ms)"), "{t}");
    }

    /// With an epoch the clock column is a real wall-clock time derived from the
    /// monotonic instant, not a second reading of the system clock.
    #[test]
    fn an_epoch_turns_instants_into_wall_clock() {
        let c = Clock::new();
        let mut s = SerialState::default();
        push(&mut s, &c, Dir::AppToSensor, b"a", 0);
        let epoch = (c.at(0), std::time::UNIX_EPOCH + Duration::from_secs(3661));
        let t = bridge_log_job(&s.log, false, 12.0, b"", b"", true, Some(epoch), true).text;
        assert!(t.contains("01:01:01.000"), "{t}");
    }

    #[test]
    fn stamps_can_be_turned_off() {
        let c = Clock::new();
        let mut s = SerialState::default();
        push(&mut s, &c, Dir::AppToSensor, b"a", 0);
        let t = text_of(&s, false, b"", b"");
        assert!(!t.contains('['), "{t}");
        assert!(t.starts_with(">> a"), "{t}");
    }
}

#[cfg(test)]
mod tests {
    use super::{SEARCH_HIT, byte_color, render_rx_text, seq_color, seq_counts, text_search_job};
    use eframe::egui;

    /// Collect (line_text, is_yellow) from a text-search job.
    fn rows(text: &str, needle: &str) -> Vec<(String, bool)> {
        let job = text_search_job(text, needle, 12.0, egui::Color32::GRAY);
        job.sections
            .iter()
            .map(|s| {
                (
                    job.text[s.byte_range.start.0..s.byte_range.end.0].to_string(),
                    s.format.color == SEARCH_HIT,
                )
            })
            .collect()
    }

    #[test]
    fn text_search_highlights_matching_line_prefixes() {
        let out = rows("INFO ready\nERR bad\ninfo again\n", "info");
        // Case-insensitive prefix match on lines 1 and 3; line 2 stays default.
        assert_eq!(out[0], ("INFO ready\n".to_owned(), true));
        assert_eq!(out[1], ("ERR bad\n".to_owned(), false));
        assert_eq!(out[2], ("info again\n".to_owned(), true));
    }

    #[test]
    fn text_search_ignores_leading_whitespace_and_needs_a_prefix() {
        // Leading spaces don't defeat the prefix match…
        assert!(rows("   DATA=5\n", "DATA")[0].1);
        // …but a match in the MIDDLE of the line does not highlight it.
        assert!(!rows("x DATA y\n", "DATA")[0].1);
        // Text reproduced exactly (concatenated sections == input).
        let job = text_search_job("a\nb\n", "a", 12.0, egui::Color32::GRAY);
        assert_eq!(job.text, "a\nb\n");
    }

    #[test]
    fn text_mode_is_lossy_utf8() {
        assert_eq!(render_rx_text(b"hi\n"), "hi\n");
        // An invalid byte becomes the replacement char, not a panic.
        assert!(render_rx_text(&[0xff, b'a']).ends_with('a'));
    }

    #[test]
    fn colors_are_deterministic_and_distinct() {
        // Same byte / sequence → same colour; different → different.
        assert_eq!(byte_color(0x4E), byte_color(0x4E));
        assert_ne!(byte_color(0x4E), byte_color(0x4F));
        assert_eq!(seq_color(b"ON"), seq_color(b"ON"));
        assert_ne!(seq_color(b"ON"), seq_color(b"NO"));
        // Single-byte sequence matches byte_color.
        assert_eq!(seq_color(&[0x4E]), byte_color(0x4E));
    }

    #[test]
    fn parse_hex_search_groups_pairs() {
        use super::parse_hex_search;
        assert_eq!(parse_hex_search("0D 0A"), vec![0x0D, 0x0A]);
        assert_eq!(parse_hex_search("4f4e"), vec![0x4F, 0x4E]);
        assert_eq!(parse_hex_search("  "), Vec::<u8>::new());
        assert_eq!(parse_hex_search("4"), Vec::<u8>::new()); // lone digit dropped
    }

    #[test]
    fn hex_search_colors_two_patterns_rest_grey() {
        use super::{SEARCH_HIT, SEARCH_HIT2, SEARCH_MISS, hex_search_job};
        // 01 0D 0A 02: pattern A = 0D 0A (yellow), pattern B = 01 (blue).
        let job = hex_search_job(
            &[0x01, 0x0D, 0x0A, 0x02],
            12.0,
            &[(&[0x0D, 0x0A], SEARCH_HIT), (&[0x01], SEARCH_HIT2)],
            16,
        );
        // Per BYTE, not per section: from egui 0.36 on, `LayoutJob::append`
        // merges a run of same-coloured bytes ("0D 0A") into one section.
        let colors: Vec<(String, egui::Color32)> = job
            .sections
            .iter()
            .flat_map(|s| {
                job.text[s.byte_range.start.0..s.byte_range.end.0]
                    .split_whitespace()
                    .map(move |t| (t.to_string(), s.format.color))
            })
            .collect();
        assert_eq!(colors[0], ("01".into(), SEARCH_HIT2)); // blue
        assert_eq!(colors[1], ("0D".into(), SEARCH_HIT)); // yellow
        assert_eq!(colors[2], ("0A".into(), SEARCH_HIT)); // yellow
        assert_eq!(colors[3], ("02".into(), SEARCH_MISS)); // grey
    }

    /// The user's example: header `FD FC FB FA`, tail `04 03 02 01`, payload
    /// `01 02 03 04 05` → 5 bytes between the markers (markers excluded).
    #[test]
    fn gap_counts_measures_payload_between_markers() {
        use super::gap_counts;
        let a = [0xFD, 0xFC, 0xFB, 0xFA];
        let b = [0x04, 0x03, 0x02, 0x01];
        let stream = [
            0xFD, 0xFC, 0xFB, 0xFA, // Find1
            0x01, 0x02, 0x03, 0x04, 0x05, // payload — note it contains 04
            0x04, 0x03, 0x02, 0x01, // Find2
        ];
        assert_eq!(gap_counts(&stream, &a, &b), vec![5]);
        // Bytes before the first header and after the last tail are ignored.
        let noisy = [&[0xFF, 0xFF][..], &stream[..], &[0xEE][..]].concat();
        assert_eq!(gap_counts(&noisy, &a, &b), vec![5]);
    }

    #[test]
    fn gap_counts_handles_multiple_frames_and_partial_tail() {
        use super::gap_counts;
        let (a, b) = ([0xAAu8], [0xBBu8]);
        // Two complete frames (2 B and 0 B) + an unterminated third.
        let stream = [0xAA, 1, 2, 0xBB, 0xAA, 0xBB, 0xAA, 9, 9];
        assert_eq!(gap_counts(&stream, &a, &b), vec![2, 0]);
        // Empty patterns → nothing to measure.
        assert!(gap_counts(&stream, &[], &b).is_empty());
        assert!(gap_counts(&stream, &a, &[]).is_empty());
        // Same pattern both sides → gap between consecutive markers.
        assert_eq!(gap_counts(&[0xAA, 1, 2, 0xAA], &a, &a), vec![2]);
    }

    /// A truncated frame (header, no tail, header again) must measure from the
    /// SECOND header — otherwise the lost frame's bytes inflate the count.
    #[test]
    fn gap_counts_restarts_on_a_repeated_header() {
        use super::gap_counts;
        let stream = [0xAA, 7, 7, 7, 0xAA, 1, 0xBB];
        assert_eq!(gap_counts(&stream, &[0xAA], &[0xBB]), vec![1]);
    }

    #[test]
    fn seq_counts_groups_by_length_sorted_by_frequency() {
        // Single bytes (len 1): 'A'×3, 'B'×1.
        assert_eq!(
            seq_counts(b"AABA", 1),
            vec![(vec![b'A'], 3), (vec![b'B'], 1)]
        );
        // 2-byte sequences aligned from 0: "ON" "ON" "XY" → "ON"×2 first.
        assert_eq!(
            seq_counts(b"ONONXY", 2),
            vec![(b"ON".to_vec(), 2), (b"XY".to_vec(), 1)]
        );
        // Trailing partial chunk is ignored ("ON" + leftover 'X').
        assert_eq!(seq_counts(b"ONX", 2), vec![(b"ON".to_vec(), 1)]);
    }
}

#[cfg(test)]
mod baud_list_tests {
    use super::{BAUDS, port_label};

    /// Every string one frame of `picker` paints, rendered headless.
    fn painted(baud: &mut u32, picker: impl Fn(&mut eframe::egui::Ui, &mut u32)) -> Vec<String> {
        use eframe::egui;
        fn walk(s: &egui::Shape, out: &mut Vec<String>) {
            match s {
                egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let shapes = crate::headless::run_ui(&ctx, input, |ui| picker(ui, baud)).shapes;
        let mut out = Vec::new();
        for s in &shapes {
            walk(&s.shape, &mut out);
        }
        out
    }

    fn picker(ui: &mut eframe::egui::Ui, baud: &mut u32) {
        super::baud_picker(ui, "t", baud, super::BAUD_MIN..=super::BAUD_MAX, &|_| None);
    }

    /// A rate outside the presets - one a USART module seeds into the Serial
    /// tab, say - shows as "Custom" with the number in the typed field. Before
    /// the field existed, the tab could show such a rate but never get back to
    /// it once another was picked.
    #[test]
    fn a_rate_outside_the_presets_is_shown_in_the_typed_field() {
        let mut baud = 250_000;
        let texts = painted(&mut baud, picker);
        assert!(texts.iter().any(|t| t == "Custom"), "{texts:?}");
        // egui paints a DragValue's number and its suffix as two texts.
        assert!(texts.iter().any(|t| t == "250000"), "{texts:?}");
        assert!(texts.iter().any(|t| t == " baud"), "{texts:?}");
        assert_eq!(baud, 250_000, "drawing does not touch the rate");
    }

    /// A picker that once showed a custom rate goes back to the preset when
    /// the value becomes one from outside - the Serial tab re-seeded by the
    /// next project, say. The remembered choice must not latch "Custom".
    #[test]
    fn a_custom_rate_does_not_latch_the_picker() {
        use eframe::egui;
        let ctx = egui::Context::default();
        let frame = |baud: &mut u32| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            };
            let shapes = crate::headless::run_ui(&ctx, input, |ui| picker(ui, baud)).shapes;
            let mut out = Vec::new();
            for s in &shapes {
                if let egui::Shape::Text(t) = &s.shape {
                    out.push(t.galley.text().to_owned());
                }
            }
            out
        };
        let mut baud = 250_000;
        assert!(frame(&mut baud).iter().any(|t| t == "Custom"));
        baud = 115_200;
        let texts = frame(&mut baud);
        assert!(texts.iter().any(|t| t == "115200"), "{texts:?}");
        assert!(!texts.iter().any(|t| t == "Custom"), "{texts:?}");
    }

    /// A preset reads as itself, with no typed field beside it.
    #[test]
    fn a_preset_shows_no_typed_field() {
        let mut baud = 115_200;
        let texts = painted(&mut baud, picker);
        assert!(texts.iter().any(|t| t == "115200"), "{texts:?}");
        assert!(!texts.iter().any(|t| t.ends_with(" baud")), "{texts:?}");
    }

    /// A stored rate outside the typed range is REPORTED, not rewritten: the
    /// widget must not clamp a hand-edited `mcu.config` on sight, because the
    /// rate is hashed into generated code.
    #[test]
    fn a_stored_rate_outside_the_range_is_left_alone() {
        let mut baud = 100;
        let texts = painted(&mut baud, picker);
        assert_eq!(baud, 100);
        assert!(texts.iter().any(|t| t == "100"), "{texts:?}");
        assert!(texts.iter().any(|t| t == " baud"), "{texts:?}");
    }

    /// The default a fresh console starts at has to be selectable, or the very
    /// first thing the combo shows is a value it cannot get back to.
    #[test]
    fn the_default_baud_is_in_the_list() {
        assert!(BAUDS.contains(&115_200), "{BAUDS:?}");
    }

    /// A USART Virtual Module's default too - that is the value the Serial tab
    /// is SEEDED with the first time it opens, so the two must agree.
    #[test]
    fn the_seed_from_a_usart_module_is_selectable() {
        let seeded = crate::panels::mcu_module::modules::model::UsartModuleConfig::new(1).baud_rate;
        assert!(
            BAUDS.contains(&seeded),
            "a module seeds {seeded} baud and the picker cannot show it: {BAUDS:?}"
        );
    }

    /// Sorted and unique: the combo renders in order, and a repeat would draw

    /// A port is named from the table the Flash tab already uses, and falls back
    /// to the descriptor's own strings - never to nothing when the OS said
    /// something. A port the OS does not call USB has no honest label.
    #[test]
    fn a_usb_port_is_named_by_the_table_then_by_its_descriptor() {
        use serialport::{SerialPortInfo, SerialPortType, UsbPortInfo};
        let usb = |vid, pid, product: Option<&str>| SerialPortInfo {
            port_name: "COM7".to_owned(),
            port_type: SerialPortType::UsbPort(UsbPortInfo {
                vid,
                pid,
                serial_number: None,
                manufacturer: None,
                product: product.map(str::to_owned),
            }),
        };
        // The chip's own USB peripheral, which this tab used to render as a bare
        // COM number while the Flash tab spelled it out.
        assert_eq!(
            port_label(&usb(0x303a, 0x1001, Some("USB JTAG/serial debug unit"))).as_deref(),
            Some("Espressif built-in USB-Serial/JTAG")
        );
        // The bridge every esp32 / esp32c2 / esp32s2 board reaches the host by.
        assert_eq!(
            port_label(&usb(0x10c4, 0xea60, None)).as_deref(),
            Some("CP2102 USB-Serial")
        );
        // An id the table has NOT heard of, from a vendor it has: the device's
        // own name, not the vendor's first row. `find_programmer` would answer
        // "CP2102 USB-Serial" here, which is a different chip.
        assert_eq!(
            port_label(&usb(0x10c4, 0xea7a, Some("CP2110 HID USB-to-UART"))).as_deref(),
            Some("CP2110 HID USB-to-UART")
        );
        // Unlisted Espressif with nothing to say for itself: the vendor guess is
        // the last resort, and it is still better than a bare COM number.
        assert_eq!(
            port_label(&usb(0x303a, 0x0009, None)).as_deref(),
            Some("Espressif USB device")
        );
        // An id the table has never heard of still says what it is.
        assert_eq!(
            port_label(&usb(0x1234, 0x5678, Some("Widget"))).as_deref(),
            Some("Widget")
        );
        assert_eq!(port_label(&usb(0x1234, 0x5678, None)), None);
        assert_eq!(
            port_label(&SerialPortInfo {
                port_name: "COM1".to_owned(),
                port_type: SerialPortType::Unknown,
            }),
            None
        );
    }

    /// the same row twice.
    #[test]
    fn the_list_is_sorted_and_has_no_repeats() {
        assert!(BAUDS.windows(2).all(|w| w[0] < w[1]), "{BAUDS:?}");
    }

    /// The Espressif boot-log rate on a 26 MHz part, which is the reason this
    /// list is not just the eight round numbers. 115200 x 26 / 40 = 74880
    /// exactly, so the arithmetic is checkable even though the ROM's divisor
    /// choice is not visible from here.
    #[test]
    fn the_26mhz_boot_log_rate_is_offered() {
        assert_eq!(115_200 * 26 / 40, 74_880);
        assert!(
            BAUDS.contains(&74_880),
            "an esp32 or esp32c2 boot log is unreadable at every other rate"
        );
    }

    /// WHICH parts the rate is for, in both directions: the bundled definitions
    /// that offer a 26 MHz crystal are exactly `esp32` and `esp32c2`. The old
    /// version only asked whether `esp32c2` was among them, so it would still
    /// have passed if a part had lost the option or a new one had gained it -
    /// and the comment on BAUDS names a SET, not a member.
    #[test]
    fn the_only_26mhz_parts_are_the_two_this_rate_is_for() {
        use crate::panels::mcu_module::builtins::builtin_definitions;
        use crate::panels::mcu_module::clock::graph::model::NodeKind;
        use crate::panels::mcu_module::clock::model::ClockConfig;

        let mut found = Vec::new();
        for d in builtin_definitions() {
            let mcu = d.build_mcu();
            let ClockConfig::Graph(gc) = &mcu.clock else {
                continue;
            };
            // The crystal is a RANGE, and 26 MHz is where the C2's starts -
            // which is also the value its state defaults to.
            let has26 = gc.graph.nodes.iter().any(|n| {
                n.id == "xtal"
                    && matches!(&n.kind, NodeKind::Source { min_hz, .. } if *min_hz == 26_000_000)
            });
            if has26 {
                found.push(d.id.clone());
            }
        }
        found.sort();
        assert_eq!(
            found,
            ["esp32", "esp32c2"],
            "the 26 MHz set moved; 74880 is the boot-log rate for exactly it, and the comment on BAUDS names it"
        );
    }
}
