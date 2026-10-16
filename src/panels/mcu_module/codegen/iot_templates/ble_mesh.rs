
// Everything below is editable — your changes are preserved on regeneration.
//
// From anywhere in the firmware:
//
//     use pins::configs::ble_mesh;
//     ble_mesh::onoff_set(true).await.ok();        // a switch: its group's lights on
//     let set = ble_mesh::onoff_changed().await;   // a light: set.on, from set.src
//     ble_mesh::sensor_set(0x004F, 21.5).await.ok(); // a sensor's value (degC)
//     let r = ble_mesh::sensor_reading().await;    // a reader: r.value(), from r.src
//
// A Bluetooth Mesh node the IDE provisioned: the network's NetKey and AppKey
// and the node's DevKey are in secrets.rs, its address and models above. Copy
// the network ("Copy network" in the IoT tab) into every other project of the
// network; give each node its own address. The models talk in groups: a
// switch sends to ONOFF_GROUP, the lights listen there and publish their state
// back to it; the sensors publish to SENSOR_GROUP every SENSOR_PERIOD_S.
//
// The bearer: ONE passive scan that never stops (30 ms windows, duplicates
// reported - a mesh node repeats each PDU on purpose), and each PDU advertised
// ADV_NONCONN_IND at 20 ms for a few events. A controller that refuses to
// advertise while it scans gets its scan stopped around each PDU instead.
//
// The node never sends a sequence number twice: it keeps the next free one in
// the flash store (an item of its own, key KEY_MESH, beside your `Data`),
// written ahead every SEQ_STORE_RATE PDUs. Nothing is sent until the first
// one is written. What a reset loses is the list of what the node already
// took in (in RAM): a node replays nothing, but another could replay to it.

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use bt_hci::cmd::le::{LeSetAdvData, LeSetAdvEnable, LeSetAdvParams, LeSetScanEnable, LeSetScanParams};
use bt_hci::param::{
    AdvChannelMap, AdvFilterPolicy, AdvKind, LeAdvEventKind, LeAdvReportsIter, LeScanKind,
    ScanningFilterPolicy,
};
use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use embedded_storage::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use sequential_storage::cache::{Cache, Uncached};
use sequential_storage::map::{MapConfig, MapStorage, PostcardValue};
use serde::{Deserialize, Serialize};
use trouble_host::prelude::{
    AddrKind, Address, BdAddr, BleHostError, DefaultPacketPool, EventHandler, ExternalController,
    Host, HostResources, Runner, Stack as BleStack,
};

use super::flash_store::STORE_RANGE;
use super::secrets::{MESH_APP_KEY, MESH_DEV_KEY, MESH_NET_KEY};

// ── The node, as the block above and secrets.rs make it ───────────────────

/// Company, product and version ids of the composition data: 0x05F1 is The
/// Linux Foundation's, the one Zephyr's samples use. A product needs its own.
const CID: u16 = 0x05F1;
const PID: u16 = 0x0001;
const VID: u16 = 0x0001;

/// Every node's: the keys (a malformed one fails the build, in `hex16`).
static SETUP: Setup = Setup {
    address: ADDRESS,
    iv_index: IV_INDEX,
    net_key: hex16(MESH_NET_KEY),
    app_key: hex16(MESH_APP_KEY),
    dev_key: hex16(MESH_DEV_KEY),
    default_ttl: DEFAULT_TTL,
    relay: RELAY,
    cid: CID,
    pid: PID,
    vid: VID,
    models: &MODELS,
};

const MODEL_COUNT: usize = HAS_ONOFF_SERVER as usize
    + HAS_ONOFF_CLIENT as usize
    + 2 * HAS_SENSOR_SERVER as usize
    + HAS_SENSOR_CLIENT as usize;
const ONOFF_SUBS: &[u16] = &[ONOFF_GROUP];
const SENSOR_SUBS: &[u16] = &[SENSOR_GROUP];

/// The models after the Configuration Server, each in its group: a server
/// listens there and publishes there, a client sends there and hears there.
static MODELS: [ModelSetup; MODEL_COUNT] = models();

const fn models() -> [ModelSetup; MODEL_COUNT] {
    let none = ModelSetup { id: 0, publication: Publication::NONE, subscribe: &[] };
    let mut out = [none; MODEL_COUNT];
    let mut n = 0;
    if HAS_ONOFF_SERVER {
        out[n] = ModelSetup { id: ONOFF_SERVER, publication: to_group(ONOFF_GROUP, 0), subscribe: ONOFF_SUBS };
        n += 1;
    }
    if HAS_ONOFF_CLIENT {
        out[n] = ModelSetup { id: ONOFF_CLIENT, publication: to_group(ONOFF_GROUP, 0), subscribe: ONOFF_SUBS };
        n += 1;
    }
    if HAS_SENSOR_SERVER {
        let period = period_of(SENSOR_PERIOD_S);
        out[n] = ModelSetup { id: SENSOR_SERVER, publication: to_group(SENSOR_GROUP, period), subscribe: SENSOR_SUBS };
        // The specification asks for the Setup Server beside it.
        out[n + 1] = ModelSetup { id: SENSOR_SETUP_SERVER, publication: Publication::NONE, subscribe: &[] };
        n += 2;
    }
    if HAS_SENSOR_CLIENT {
        out[n] = ModelSetup { id: SENSOR_CLIENT, publication: to_group(SENSOR_GROUP, 0), subscribe: SENSOR_SUBS };
        n += 1;
    }
    let _ = n;
    out
}

const fn to_group(address: u16, period: u8) -> Publication {
    Publication { address, ttl: 0xFF, period, retransmit: 0, app_index: 0, credentials: false }
}

/// Seconds as a Publish Period: steps of 1 s up to 63 s, of 10 s up to
/// 630 s, then of 10 minutes.
const fn period_of(s: u16) -> u8 {
    if s == 0 {
        0
    } else if s <= 63 {
        0x40 | s as u8
    } else if s <= 630 {
        0x80 | (s / 10) as u8
    } else {
        let m = s / 600;
        0xC0 | (if m > 63 { 63 } else { m }) as u8
    }
}

// ── What the board's code calls ───────────────────────────────────────────

/// Why a call did not go out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeshError {
    /// That model is off in the IoT tab.
    NoModel,
    /// The node is starting: its sequence number is not in flash yet (or the
    /// flash refused it - `stats().store_failures`).
    NotReady,
    /// Segmented messages are already on their way: try again shortly.
    Busy,
    /// More than the node sends in one message.
    TooLong,
    /// A Config Node Reset took the node off the network until the next
    /// boot.
    Removed,
}

/// A Generic OnOff Set the light took, from `src`.
#[derive(Clone, Copy, Debug)]
pub struct OnOffSet {
    pub src: u16,
    pub on: bool,
}

/// A Generic OnOff Status a switch heard: `src` is on or off.
#[derive(Clone, Copy, Debug)]
pub struct OnOffStatus {
    pub src: u16,
    pub on: bool,
}

/// One property of a Sensor Status, from `src`.
#[derive(Clone, Copy, Debug)]
pub struct SensorReading {
    pub src: u16,
    pub property: u16,
    pub raw: SensorValue,
}

impl SensorReading {
    /// The value in the property's unit (degC, %, lux, people, V); `None`
    /// when the sensor said "not known", has no such property, or it is one
    /// the IDE does not list.
    pub fn value(&self) -> Option<f32> {
        self.raw.decode(property_format(self.property)?)
    }
}

/// The Generic OnOff Server's state.
pub fn onoff() -> bool {
    ONOFF.load(Ordering::Relaxed)
}

/// The next Set the light took from the network.
pub async fn onoff_changed() -> OnOffSet {
    ONOFF_SETS.receive().await
}

/// The light changed by the board itself (a button on it): its state, and
/// the Status published to ONOFF_GROUP.
pub async fn set_onoff(on: bool) -> Result<(), MeshError> {
    call(Command::OnOffLocal(on)).await
}

/// The switch: every light of ONOFF_GROUP on or off (Set Unacknowledged; the
/// lights publish their new state, which `onoff_status` hears).
pub async fn onoff_set(on: bool) -> Result<(), MeshError> {
    call(Command::OnOffSet { dst: None, on, ack: false }).await
}

/// The switch: one light, by address, acknowledged - its Status comes to
/// `onoff_status`.
pub async fn onoff_set_to(address: u16, on: bool) -> Result<(), MeshError> {
    call(Command::OnOffSet { dst: Some(address), on, ack: true }).await
}

/// The switch: ask one light (or with `None` the whole group) for its state.
pub async fn onoff_get(address: Option<u16>) -> Result<(), MeshError> {
    call(Command::OnOffGet { dst: address }).await
}

/// The next Generic OnOff Status heard.
pub async fn onoff_status() -> OnOffStatus {
    ONOFF_STATUSES.receive().await
}

/// The Sensor Server: a property's value now (`f32::NAN`: not known). It
/// goes out with the next publication, or `sensor_publish`.
pub async fn sensor_set(property: u16, value: f32) -> Result<(), MeshError> {
    call(Command::SensorSet { property, value }).await
}

/// The Sensor Server: publish every value now, to SENSOR_GROUP.
pub async fn sensor_publish() -> Result<(), MeshError> {
    call(Command::SensorPublish).await
}

/// The Sensor Client: ask one server (or with `None` the whole group) for one
/// property (or with `None` all of them). The answers come to
/// `sensor_reading`.
pub async fn sensor_get(address: Option<u16>, property: Option<u16>) -> Result<(), MeshError> {
    call(Command::SensorGet { dst: address, property }).await
}

/// The next sensor value heard - asked for, or published by a server.
pub async fn sensor_reading() -> SensorReading {
    SENSOR_READINGS.receive().await
}

/// This node's address.
pub fn address() -> u16 {
    ADDRESS
}

/// What the node and its bearer did so far.
#[derive(Clone, Copy, Debug)]
pub struct Stats {
    /// Mesh PDUs heard, and dropped because the node fell behind.
    pub rx_frames: u32,
    pub rx_dropped: u32,
    /// PDUs advertised, and the ones a radio command failed on.
    pub tx_done: u32,
    pub tx_failed: u32,
    /// The controller cannot advertise while it scans: the scan stops
    /// around each PDU.
    pub exclusive: bool,
    /// The sequence number in use, and the flash writes that failed.
    pub seq: u32,
    pub store_failures: u32,
    /// The host started over (a controller error).
    pub host_restarts: u32,
}

pub fn stats() -> Stats {
    let get = |c: &AtomicU32| c.load(Ordering::Relaxed);
    Stats {
        rx_frames: get(&RX_FRAMES),
        rx_dropped: get(&RX_DROPPED),
        tx_done: get(&TX_DONE),
        tx_failed: get(&TX_FAILED),
        exclusive: EXCLUSIVE.load(Ordering::Relaxed),
        seq: get(&SEQ),
        store_failures: get(&STORE_FAILURES),
        host_restarts: get(&HOST_RESTARTS),
    }
}

/// Starts the task that owns Bluetooth and the node, on the radio `main.rs`
/// switched on, with the flash the store shares.
///
/// `mac`: `None` keeps the radio's own (public) address; `Some` gives the
/// board a random static address made of it.
pub fn start(spawner: Spawner, radio: Radio, mac: Option<[u8; 6]>, flash: Flash) {
    spawner.spawn(mesh_task(ExternalController::new(radio), mac.map(random_static), flash).unwrap());
}

/// A random static address made of `mac`: least significant byte first, the
/// top two bits set.
fn random_static(mac: [u8; 6]) -> Address {
    let mut bytes = mac;
    bytes.reverse();
    bytes[5] |= 0xC0;
    Address::random(bytes)
}

// ── Between the calls and the node ────────────────────────────────────────

enum Command {
    OnOffLocal(bool),
    OnOffSet { dst: Option<u16>, on: bool, ack: bool },
    OnOffGet { dst: Option<u16> },
    SensorSet { property: u16, value: f32 },
    SensorPublish,
    SensorGet { dst: Option<u16>, property: Option<u16> },
}

/// One call at a time: its command, then its answer - both numbered, so a
/// call dropped half way (a timeout around it) never hands its answer to
/// the next one.
static CALL: Mutex<CriticalSectionRawMutex, ()> = Mutex::new(());
static COMMAND: Channel<CriticalSectionRawMutex, (u32, Command), 1> = Channel::new();
static ANSWER: Signal<CriticalSectionRawMutex, (u32, Result<(), MeshError>)> = Signal::new();
/// The last call's number. One writer: the caller holding CALL.
static CALLS: AtomicU32 = AtomicU32::new(0);

async fn call(c: Command) -> Result<(), MeshError> {
    let _turn = CALL.lock().await;
    let id = CALLS.load(Ordering::Relaxed).wrapping_add(1);
    CALLS.store(id, Ordering::Relaxed);
    COMMAND.send((id, c)).await;
    loop {
        let (answered, r) = ANSWER.wait().await;
        if answered == id {
            return r;
        }
    }
}

/// What the node heard for the board's code: dropped when nobody reads.
static ONOFF_SETS: Channel<CriticalSectionRawMutex, OnOffSet, 4> = Channel::new();
static ONOFF_STATUSES: Channel<CriticalSectionRawMutex, OnOffStatus, 4> = Channel::new();
/// A whole Sensor Status fits: up to SENSORS_MAX readings at once.
static SENSOR_READINGS: Channel<CriticalSectionRawMutex, SensorReading, SENSORS_MAX> = Channel::new();
static ONOFF: AtomicBool = AtomicBool::new(false);

/// Network PDUs heard, and to advertise.
static RX: Channel<CriticalSectionRawMutex, Frame, RX_DEPTH> = Channel::new();
static TX: Channel<CriticalSectionRawMutex, Transmission, TX_DEPTH> = Channel::new();
/// Raised when the host starts over: the controller was reset, the scan has
/// to be set up again.
static RESCAN: Signal<CriticalSectionRawMutex, ()> = Signal::new();

// Counters. One writer each - the mesh task - so a load and a store make the
// increment: no read-modify-write atomics, which the RP2040's Cortex-M0+ and
// the ESP32-C3 do not have.
static RX_FRAMES: AtomicU32 = AtomicU32::new(0);
static RX_DROPPED: AtomicU32 = AtomicU32::new(0);
static TX_DONE: AtomicU32 = AtomicU32::new(0);
static TX_FAILED: AtomicU32 = AtomicU32::new(0);
static SEQ: AtomicU32 = AtomicU32::new(0);
static STORE_FAILURES: AtomicU32 = AtomicU32::new(0);
static HOST_RESTARTS: AtomicU32 = AtomicU32::new(0);
static EXCLUSIVE: AtomicBool = AtomicBool::new(false);

fn bump(counter: &AtomicU32) {
    counter.store(counter.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
}

// ── The flash the store and the node share ────────────────────────────────

static FLASH: BlockingMutex<CriticalSectionRawMutex, RefCell<Option<StoreFlash>>> =
    BlockingMutex::new(RefCell::new(None));

/// The flash store's flash, shared: `main.rs` makes it once (`share`), then
/// hands a copy to the store and one to the node. Each operation runs whole
/// in a critical section, so neither ever sees the other's half done.
#[derive(Clone, Copy)]
pub struct Flash(());

impl Flash {
    pub fn share(flash: StoreFlash) -> Self {
        FLASH.lock(|f| f.replace(Some(flash)));
        Flash(())
    }

    fn with<R>(&self, op: impl FnOnce(&mut StoreFlash) -> R) -> R {
        // `Flash` exists only once `share` filled it.
        FLASH.lock(|f| op(f.borrow_mut().as_mut().expect("Flash::share runs first")))
    }
}

impl ErrorType for Flash {
    type Error = <StoreFlash as ErrorType>::Error;
}

impl ReadNorFlash for Flash {
    const READ_SIZE: usize = <StoreFlash as ReadNorFlash>::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.with(|f| ReadNorFlash::read(f, offset, bytes))
    }

    fn capacity(&self) -> usize {
        self.with(|f| ReadNorFlash::capacity(f))
    }
}

impl NorFlash for Flash {
    const WRITE_SIZE: usize = <StoreFlash as NorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <StoreFlash as NorFlash>::ERASE_SIZE;

    /// A sector at a time, each in its own critical section: interrupts
    /// (the radio's) come back between sectors, as with the store's own
    /// driver.
    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let mut at = from;
        while at < to {
            let end = (at + Self::ERASE_SIZE as u32).min(to);
            self.with(|f| f.erase(at, end))?;
            at = end;
        }
        Ok(())
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.with(|f| NorFlash::write(f, offset, bytes))
    }
}

// Both flash store drivers allow writing a word twice (sequential-storage's
// `remove_item` needs it): so does the handle over them.
impl MultiwriteNorFlash for Flash {}

/// The node's item in the flash store's map. Your `Data` is key 1.
const KEY_MESH: u8 = 0xB0;

#[derive(Serialize, Deserialize)]
struct SeqRecord {
    version: u8,
    /// No SEQ at or past this one was ever sent.
    seq: u32,
}

impl<'a> PostcardValue<'a> for SeqRecord {}

/// The map's buffer: a whole page, so any item the map holds fits - your
/// `Data` grows as you like, and the node walks past it (or carries it
/// along when a page is reclaimed).
const SEQ_BUF: usize = <StoreFlash as NorFlash>::ERASE_SIZE;

#[repr(align(4))]
struct SeqBuf([u8; SEQ_BUF]);

type NoCache = Cache<Uncached, Uncached, Uncached, u8>;

struct SeqStore {
    map: MapStorage<u8, BlockingAsync<Flash>, NoCache>,
    buf: SeqBuf,
}

impl SeqStore {
    fn new(flash: Flash) -> Self {
        Self {
            map: MapStorage::new(
                BlockingAsync::new(flash),
                const { MapConfig::<BlockingAsync<Flash>>::new(STORE_RANGE) },
                Cache::new_uncached(),
            ),
            buf: SeqBuf([0; SEQ_BUF]),
        }
    }

    /// The SEQ the node may start from. `Ok(None)`: the first boot - no
    /// record, one of another version, or a range holding something else
    /// (the first save formats it). `Err`: the store could not be read; the
    /// node then waits, since starting from 0 would send numbers it sent.
    async fn load(&mut self) -> Result<Option<u32>, ()> {
        match self.map.fetch_item::<SeqRecord>(&mut self.buf.0, &KEY_MESH).await {
            Ok(Some(r)) if r.version == 1 => Ok(Some(r.seq)),
            Ok(_) => Ok(None),
            Err(sequential_storage::Error::Corrupted { .. }) => Ok(None),
            Err(_) => Err(()),
        }
    }

    async fn save(&mut self, seq: u32) -> bool {
        let r = SeqRecord { version: 1, seq };
        match self.map.store_item(&mut self.buf.0, &KEY_MESH, &r).await {
            Err(sequential_storage::Error::Corrupted { .. }) => {
                // What flash_store.rs's `save` does too: a range holding
                // something else is formatted once.
                self.map.erase_all().await.is_ok() && self.map.store_item(&mut self.buf.0, &KEY_MESH, &r).await.is_ok()
            }
            other => other.is_ok(),
        }
    }
}

// ── The task ──────────────────────────────────────────────────────────────

/// The advertising bearer opens no connection; one slot keeps the host's
/// tables non-empty.
const CONNECTIONS: usize = 1;
/// L2CAP channels: none used.
const CHANNELS: usize = 1;
/// HCI commands that may wait for the controller at once.
const SLOTS: usize = 20;
/// PDUs heard and not yet taken in: a full queue drops the newest.
const RX_DEPTH: usize = 8;
/// PDUs waiting for the bearer.
const TX_DEPTH: usize = 4;
/// AD type of a Mesh Message: a Network PDU.
const AD_MESH_MESSAGE: u8 = 0x2A;
/// Scan interval = window = 30 ms (Zephyr's), in the controller's 0.625 ms
/// units. Raw on purpose: bt-hci 0.8.1 declares `LeSetScanParams`'s times in
/// 10 ms units, so `from_millis(30)` would put 1.875 ms on the wire.
const SCAN_UNITS_625US: u16 = 48;
/// The shortest non-connectable advertising interval (Bluetooth 5.0).
const ADV_INTERVAL_MIN_MS: u16 = 20;

type Controller = ExternalController<Radio, SLOTS>;

/// A Network PDU heard.
#[derive(Clone, Copy)]
struct Frame {
    len: u8,
    data: [u8; NET_MAX],
}

#[embassy_executor::task]
async fn mesh_task(controller: Controller, address: Option<Address>, flash: Flash) -> ! {
    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS, CHANNELS> = HostResources::new();
    let own = address.map_or(AddrKind::PUBLIC, |a| a.kind);
    let stack = trouble_host::new(controller, &mut resources);
    let stack = match address {
        Some(address) => stack.set_random_address(address),
        None => stack,
    };
    let Host { runner, .. } = stack.build();
    let mut store = SeqStore::new(flash);
    match select3(run_host(runner), bearer(&stack, own), node(&mut store)).await {
        Either3::First(never) | Either3::Second(never) | Either3::Third(never) => never,
    }
}

/// Runs the host: every HCI event goes through here, advertising reports to
/// `MeshReports`.
async fn run_host(mut runner: Runner<'_, Controller, DefaultPacketPool>) -> ! {
    loop {
        // Returns only on a controller error; running again resets it.
        let _ = runner.run_with_handler(&MeshReports).await;
        bump(&HOST_RESTARTS);
        Timer::after(Duration::from_secs(1)).await;
        RESCAN.signal(());
    }
}

/// Picks the Mesh Messages out of the reports. Runs inside the host's
/// receive loop: it must not wait, so a full queue drops the frame.
struct MeshReports;

impl EventHandler for MeshReports {
    fn on_adv_reports(&self, reports: LeAdvReportsIter<'_>) {
        for report in reports {
            let Ok(report) = report else {
                break;
            };
            if report.event_kind != LeAdvEventKind::AdvNonconnInd {
                continue;
            }
            for (ad_type, body) in ad_structures(report.data) {
                if ad_type != AD_MESH_MESSAGE || body.is_empty() || body.len() > NET_MAX {
                    continue;
                }
                let mut frame = Frame { len: body.len() as u8, data: [0; NET_MAX] };
                frame.data[..body.len()].copy_from_slice(body);
                if RX.try_send(frame).is_ok() {
                    bump(&RX_FRAMES);
                } else {
                    bump(&RX_DROPPED);
                }
            }
        }
    }
}

/// The AD structures of an advertisement: (type, body).
fn ad_structures(mut data: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    core::iter::from_fn(move || {
        let (&len, rest) = data.split_first()?;
        let len = usize::from(len);
        if len == 0 || len > rest.len() {
            data = &[];
            return None;
        }
        let (ad, tail) = rest.split_at(len);
        data = tail;
        Some((ad[0], &ad[1..]))
    })
}

/// The scan, then the PDUs one after the other; both again after the host
/// started over.
async fn bearer(stack: &BleStack<'_, Controller, DefaultPacketPool>, own: AddrKind) -> ! {
    loop {
        // `command` waits until the host has initialised the controller.
        while !start_scan(stack, own).await {
            Timer::after(Duration::from_secs(1)).await;
        }
        loop {
            match select(TX.receive(), RESCAN.wait()).await {
                Either::First(t) => {
                    if transmit(stack, own, &t).await {
                        bump(&TX_DONE);
                    } else {
                        bump(&TX_FAILED);
                    }
                }
                // The controller was reset with the host: give it time
                // before the scan is set up again.
                Either::Second(()) => {
                    Timer::after(Duration::from_millis(500)).await;
                    break;
                }
            }
        }
    }
}

/// Passive scan, interval = window = 30 ms, duplicates reported.
async fn start_scan(stack: &BleStack<'_, Controller, DefaultPacketPool>, own: AddrKind) -> bool {
    let _ = stack.command(LeSetScanEnable::new(false, false)).await;
    let params = LeSetScanParams::new(
        LeScanKind::Passive,
        bt_hci::param::Duration::from_u16(SCAN_UNITS_625US),
        bt_hci::param::Duration::from_u16(SCAN_UNITS_625US),
        own,
        ScanningFilterPolicy::BasicUnfiltered,
    );
    stack.command(params).await.is_ok() && stack.command(LeSetScanEnable::new(true, false)).await.is_ok()
}

/// 0..20 ms before each PDU: nodes answering one group request do not all
/// start on the same advertising event. A xorshift seeded from the address
/// and the clock - for spreading, not for keys.
static JITTER: AtomicU32 = AtomicU32::new(0);

fn jitter_ms() -> u64 {
    let mut x = JITTER.load(Ordering::Relaxed);
    if x == 0 {
        x = ((u32::from(ADDRESS) << 16) ^ (Instant::now().as_micros() as u32)) | 1;
    }
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    JITTER.store(x, Ordering::Relaxed);
    u64::from(x % 21)
}

/// One PDU, beside the running scan - or with the scan stopped around it, on
/// a controller that cannot do both.
async fn transmit(stack: &BleStack<'_, Controller, DefaultPacketPool>, own: AddrKind, t: &Transmission) -> bool {
    Timer::after(Duration::from_millis(jitter_ms())).await;
    let mut ad = [0u8; 31];
    let n = 2 + t.len;
    ad[0] = t.len as u8 + 1;
    ad[1] = AD_MESH_MESSAGE;
    ad[2..n].copy_from_slice(&t.pdu[..t.len]);
    let interval = t.interval_ms.max(ADV_INTERVAL_MIN_MS);
    if !EXCLUSIVE.load(Ordering::Relaxed) {
        match advertise_once(stack, own, &ad[..n], interval, t.transmits).await {
            Err(Refused::Disallowed) => EXCLUSIVE.store(true, Ordering::Relaxed),
            done => return done.is_ok(),
        }
    }
    if stack.command(LeSetScanEnable::new(false, false)).await.is_err() {
        return false;
    }
    let sent = advertise_once(stack, own, &ad[..n], interval, t.transmits).await.is_ok();
    let scanning = stack.command(LeSetScanEnable::new(true, false)).await.is_ok();
    sent && scanning
}

/// ADV_NONCONN_IND on all three channels, `interval_ms` apart, for about
/// `transmits` events, then off. Legacy advertising counts no events: each
/// lasts the interval plus the controller's random 0..10 ms, so it is timed.
async fn advertise_once(
    stack: &BleStack<'_, Controller, DefaultPacketPool>,
    own: AddrKind,
    ad: &[u8],
    interval_ms: u16,
    transmits: u8,
) -> Result<(), Refused> {
    let interval = bt_hci::param::Duration::from_micros(u64::from(interval_ms) * 1000);
    let params = LeSetAdvParams::new(
        interval,
        interval,
        AdvKind::AdvNonconnInd,
        own,
        AddrKind::PUBLIC,
        BdAddr::default(),
        AdvChannelMap::ALL,
        AdvFilterPolicy::Unfiltered,
    );
    refused(stack.command(params).await)?;
    let mut data = [0u8; 31];
    data[..ad.len()].copy_from_slice(ad);
    refused(stack.command(LeSetAdvData::new(ad.len() as u8, data)).await)?;
    refused(stack.command(LeSetAdvEnable::new(true)).await)?;
    let hold = u64::from(transmits.max(1)) * (u64::from(interval_ms) + 10);
    Timer::after(Duration::from_millis(hold)).await;
    refused(stack.command(LeSetAdvEnable::new(false)).await)?;
    Ok(())
}

/// Why the controller did not advertise.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Refused {
    /// "Command Disallowed": what a controller answers for a state it cannot
    /// combine with the current one - here, advertising while scanning.
    Disallowed,
    Failed,
}

fn refused<T, E>(r: Result<T, BleHostError<E>>) -> Result<T, Refused> {
    r.map_err(|e| match &e {
        BleHostError::BleHost(trouble_host::Error::Hci(code)) if *code == bt_hci::param::Error::CMD_DISALLOWED => {
            Refused::Disallowed
        }
        _ => Refused::Failed,
    })
}

fn now_ms() -> u64 {
    Instant::now().as_millis()
}

/// The node: PDUs heard, the board's calls and the timers in; PDUs to the
/// bearer and events to the board's code out.
async fn node(store: &mut SeqStore) -> ! {
    // Nothing is sent before the store was read.
    let stored = loop {
        match store.load().await {
            Ok(seq) => break seq,
            Err(()) => {
                bump(&STORE_FAILURES);
                Timer::after(Duration::from_secs(2)).await;
            }
        }
    };
    let mut mesh = Mesh::new(&SETUP, SENSOR_PROPERTIES, stored);
    // The switch's TID from somewhere new after a reset: its first Set is
    // then not taken for the last one before it.
    mesh.models.tid = jitter_ms() as u8 ^ (Instant::now().as_micros() as u8);
    // A failed SEQ store is tried again later, backing off to a minute: it
    // may erase a sector each time, and the node keeps hearing meanwhile.
    let mut retry_at: Option<u64> = None;
    let mut backoff_ms: u64 = 1_000;
    loop {
        let now = now_ms();
        let next = mesh.poll(now);
        // The SEQ ahead of use first: nothing is sent past what is stored.
        if let Some(seq) = mesh.stack.seq_to_store() {
            let due = match retry_at {
                Some(at) => now >= at,
                None => true,
            };
            if due {
                if store.save(seq).await {
                    mesh.stack.seq_stored(seq);
                    retry_at = None;
                    backoff_ms = 1_000;
                } else {
                    bump(&STORE_FAILURES);
                    retry_at = Some(now + backoff_ms);
                    backoff_ms = (backoff_ms * 2).min(60_000);
                }
            }
        }
        SEQ.store(mesh.stack.seq(), Ordering::Relaxed);
        drain(&mut mesh);
        // Waits while the bearer is behind: the node is not ahead of the air.
        while let Some(t) = mesh.stack.outbox.pop() {
            TX.send(t).await;
        }
        let wake = match (next, retry_at) {
            (Some(n), Some(r)) => n.min(r),
            (n, r) => n.or(r).unwrap_or(u64::MAX),
        };
        let wake = if wake == u64::MAX { Instant::MAX } else { Instant::from_millis(wake) };
        match select3(RX.receive(), COMMAND.receive(), Timer::at(wake)).await {
            Either3::First(f) => mesh.receive(&f.data[..usize::from(f.len)], now_ms()),
            Either3::Second((id, c)) => {
                let r = run(&mut mesh, c, now_ms());
                ANSWER.signal((id, r));
            }
            Either3::Third(()) => {}
        }
        // What that brought, to the board's code before the next poll adds
        // more: the node's queue is not unbounded.
        drain(&mut mesh);
    }
}

/// The node's events to the board's code, and the light's state as it is.
fn drain(mesh: &mut Mesh) {
    while let Some(e) = mesh.stack.next_event() {
        deliver(e);
    }
    ONOFF.store(mesh.models.on, Ordering::Relaxed);
}

fn run(mesh: &mut Mesh, c: Command, now: u64) -> Result<(), MeshError> {
    let onoff_server = mesh.stack.model_index(ONOFF_SERVER).is_some();
    let onoff_client = mesh.stack.model_index(ONOFF_CLIENT).is_some();
    let sensor_client = mesh.stack.model_index(SENSOR_CLIENT).is_some();
    let sent = match c {
        Command::OnOffLocal(on) => {
            if !onoff_server {
                return Err(MeshError::NoModel);
            }
            ONOFF.store(on, Ordering::Relaxed);
            mesh.models.onoff_local(&mut mesh.stack, on, now).map(|_| None)
        }
        Command::OnOffSet { dst, on, ack } => {
            if !onoff_client {
                return Err(MeshError::NoModel);
            }
            mesh.models.onoff_set(&mut mesh.stack, dst, on, ack, now)
        }
        Command::OnOffGet { dst } => {
            if !onoff_client {
                return Err(MeshError::NoModel);
            }
            mesh.models.onoff_get(&mut mesh.stack, dst, now)
        }
        Command::SensorSet { property, value } => {
            let server = mesh.stack.model_index(SENSOR_SERVER).is_some();
            return if server && mesh.models.sensor_set(property, value) { Ok(()) } else { Err(MeshError::NoModel) };
        }
        Command::SensorPublish => mesh.models.sensor_publish(&mut mesh.stack, now),
        Command::SensorGet { dst, property } => {
            if !sensor_client {
                return Err(MeshError::NoModel);
            }
            mesh.models.sensor_get(&mut mesh.stack, dst, property, now)
        }
    };
    match sent {
        Ok(_) => Ok(()),
        Err(SendError::SeqNotStored) => Err(MeshError::NotReady),
        Err(SendError::Busy) => Err(MeshError::Busy),
        Err(SendError::TooLong) => Err(MeshError::TooLong),
        Err(SendError::NoAddress) => Err(MeshError::NoModel),
        Err(SendError::Removed) => Err(MeshError::Removed),
    }
}

/// An event of the node, to the board's code. Dropped when nobody reads.
fn deliver(e: Event) {
    match e {
        Event::Model(ModelEvent::OnOffSet { src, on }) => {
            ONOFF.store(on, Ordering::Relaxed);
            let _ = ONOFF_SETS.try_send(OnOffSet { src, on });
        }
        Event::Model(ModelEvent::OnOffStatus { src, on }) => {
            let _ = ONOFF_STATUSES.try_send(OnOffStatus { src, on });
        }
        Event::Model(ModelEvent::Sensor { src, property, value }) => {
            let _ = SENSOR_READINGS.try_send(SensorReading { src, property, raw: value });
        }
        Event::Sent { .. } | Event::ConfigChanged | Event::Removed => {}
    }
}
