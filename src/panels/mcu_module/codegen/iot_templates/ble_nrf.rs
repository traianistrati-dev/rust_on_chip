
// Everything below is editable — your changes are preserved on regeneration.
//
// From anywhere in the firmware:
//
//     pins::configs::ble::send(b"21.5\n").await.ok();
//     let packet = pins::configs::ble::receive().await;   // packet.data
//     let up = pins::configs::ble::connected();
//
// A GATT peripheral with the Nordic UART Service: a phone app that speaks it
// (nRF Connect, nRF Toolbox's UART, Serial Bluetooth Terminal) finds the board
// as DEVICE_NAME, subscribes to TX and writes to RX. One task owns Bluetooth:
// it serves one central at a time and advertises again once it is gone.
// Nothing is paired or encrypted - any phone in range can connect.
//
// The central reads what `send` queued in pieces of its ATT MTU less 3 bytes:
// 20 until it asks for a larger MTU, as most apps do right after connecting.
// What it writes to RX in one go must fit that size as well.
//
// The radio is Nordic's SoftDevice Controller, run by the MPSL that `main.rs`
// starts. From then on the MPSL owns RTC0, TIMER0, TEMP, the RADIO, the ECB,
// CCM and AAR blocks, PPI channels 17 to 31 and interrupt priority 0 (the
// nRF54L: GRTC channels 7 to 11, TIMER10, TIMER20 and PPI channels of its own).
// `main.rs` moved every vector it binds to priority 2; one you bind yourself
// starts at 0 and must be moved too:
//
//     embassy_nrf::interrupt::InterruptExt::set_priority(
//         embassy_nrf::interrupt::SAADC, embassy_nrf::interrupt::Priority::P2);
//
// Flash must not be written through the NVMC while it runs: it stalls the CPU
// in the middle of radio events. `nrf_sdc::mpsl::Flash` schedules the writes
// between them, but only on an MPSL built with timeslots - in `main.rs`,
// `MultiprotocolServiceLayer::with_timeslots(.., SessionMem<1>)` in place of
// `new`; on the one `main.rs` builds every write returns `FlashError::Mpsl`.

use core::sync::atomic::{AtomicBool, Ordering};

use bt_hci::cmd::SyncCmd;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use nrf_sdc::SoftdeviceController;
use nrf_sdc::mpsl::MultiprotocolServiceLayer;
use nrf_sdc::vendor::ZephyrReadStaticAddrs;
use static_cell::StaticCell;
use trouble_host::prelude::{
    AdStructure, Address, Advertisement, AdvertisementParameters, AttributeServer, AttributeTable,
    BR_EDR_NOT_SUPPORTED, Characteristic, CharacteristicProp, DefaultPacketPool, GapConfig,
    GattConnection, GattConnectionEvent, GattEvent, HostResources, LE_GENERAL_DISCOVERABLE,
    PacketPool, Peripheral, Runner, Service,
};

/// Largest `send` and largest `Packet`: one notification at the biggest ATT
/// MTU this stack agrees to (247), less its 3-byte header.
pub const MAX_DATA: usize = 244;

/// The Nordic UART Service and its two characteristics.
const NUS: u128 = 0x6E40_0001_B5A3_F393_E0A9_E50E_24DC_CA9E;
/// The central writes here (with or without a response).
const NUS_RX: u128 = 0x6E40_0002_B5A3_F393_E0A9_E50E_24DC_CA9E;
/// The board notifies here.
const NUS_TX: u128 = 0x6E40_0003_B5A3_F393_E0A9_E50E_24DC_CA9E;

/// GAP and GATT services (6), then NUS: the service, RX's declaration and
/// value, TX's declaration, value and subscription (CCCD).
const ATTRIBUTES: usize = trouble_host::gap::GAP_SERVICE_ATTRIBUTE_COUNT + 6;
/// One central at a time.
const CONNECTIONS: usize = 1;
/// L2CAP channels: signalling and ATT.
const CHANNELS: usize = 2;

/// Link-layer buffer size: the host's own packet size.
const LL_PACKET: u16 = DefaultPacketPool::MTU as u16;
/// Link-layer buffers each way.
const LL_BUFFERS: u8 = 3;
/// RAM the SoftDevice Controller gets for the configuration in `start` (one
/// peripheral link, LL_BUFFERS buffers of LL_PACKET bytes each way, one
/// advertising set): TrouBLE's own nRF examples run this configuration on
/// this size. A larger setup needs more - `start` panics when it is short -
/// and nrfxlib's `sdc.h` (`SDC_MEM_*`) gives the upper bounds to add up.
pub const SDC_MEM: usize = 4720;

/// Bytes the central wrote to RX, one write each.
pub struct Packet {
    pub data: heapless::Vec<u8, MAX_DATA>,
}

/// Why `send` refused the data.
#[derive(Debug)]
pub enum SendError {
    /// More than `MAX_DATA` bytes.
    TooLarge,
}

type Controller = SoftdeviceController<'static>;
type Server<'v> = AttributeServer<'v, NoopRawMutex, DefaultPacketPool, ATTRIBUTES, CONNECTIONS>;

static OUTGOING: Channel<CriticalSectionRawMutex, heapless::Vec<u8, MAX_DATA>, 4> = Channel::new();
static INCOMING: Channel<CriticalSectionRawMutex, Packet, 4> = Channel::new();
static CONNECTED: AtomicBool = AtomicBool::new(false);

/// Queues `data` for the central. Waits only while the queue is full; it is
/// dropped when no central is connected or it did not subscribe to TX.
pub async fn send(data: &[u8]) -> Result<(), SendError> {
    let data = heapless::Vec::from_slice(data).map_err(|_| SendError::TooLarge)?;
    OUTGOING.send(data).await;
    Ok(())
}

/// The next write of the central to RX.
pub async fn receive() -> Packet {
    INCOMING.receive().await
}

/// Is a central connected right now?
pub fn connected() -> bool {
    CONNECTED.load(Ordering::Relaxed)
}

/// Brings the SoftDevice Controller up on the MPSL `main.rs` started and
/// starts the two tasks Bluetooth needs: the MPSL's own and the one that owns
/// the host. `rng` seeds the controller's keys and addresses.
pub fn start<R: rand_core::CryptoRng>(
    spawner: Spawner,
    mpsl: &'static MultiprotocolServiceLayer<'static>,
    sdc: nrf_sdc::Peripherals<'static>,
    rng: &'static mut R,
) {
    static MEM: StaticCell<nrf_sdc::Mem<SDC_MEM>> = StaticCell::new();
    let controller = nrf_sdc::Builder::new()
        .map(|b| b.support_adv().support_peripheral())
        .and_then(|b| b.peripheral_count(1))
        .and_then(|b| b.buffer_cfg(LL_PACKET, LL_PACKET, LL_BUFFERS, LL_BUFFERS))
        .and_then(|b| b.build(sdc, rng, mpsl, MEM.init(nrf_sdc::Mem::new())))
        // Fails only when SDC_MEM is smaller than the configuration needs.
        .unwrap();
    spawner.spawn(mpsl_task(mpsl).unwrap());
    spawner.spawn(ble_task(controller).unwrap());
}

/// The MPSL's low-priority work: timeslots, clock calibration, the
/// controller's own processing. Never returns.
#[embassy_executor::task]
async fn mpsl_task(mpsl: &'static MultiprotocolServiceLayer<'static>) -> ! {
    mpsl.run().await
}

#[embassy_executor::task]
async fn ble_task(controller: Controller) -> ! {
    // The controller has no public address: the board shows up under the
    // random static one Nordic burns into each chip (FICR), the same at every
    // boot.
    let found = ZephyrReadStaticAddrs::new().exec(&controller).await.unwrap();
    let static_addr = found.addr;
    let address = Address::random(static_addr.addr.into_inner());

    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS, CHANNELS> =
        HostResources::new();
    let stack = trouble_host::new(controller, &mut resources)
        .set_random_address(address)
        .build();
    let runner = stack.runner();
    let peripheral = stack.peripheral();

    let mut rx_store = [0u8; MAX_DATA];
    let mut tx_store = [0u8; MAX_DATA];
    let mut table: AttributeTable<'_, NoopRawMutex, ATTRIBUTES> = AttributeTable::new();
    // Fails only on a name over 22 bytes, which the tab does not allow.
    GapConfig::default(DEVICE_NAME).build(&mut table).unwrap();
    let mut nus = table.add_service(Service::new(NUS));
    let rx = nus
        .add_characteristic(
            NUS_RX,
            [CharacteristicProp::Write, CharacteristicProp::WriteWithoutResponse],
            [0u8; MAX_DATA],
            &mut rx_store,
        )
        .build();
    let tx = nus
        .add_characteristic(NUS_TX, [CharacteristicProp::Notify], [0u8; MAX_DATA], &mut tx_store)
        .build()
        .to_raw();
    nus.build();
    let server: Server<'_> = AttributeServer::new(table);

    match select(run_host(runner), advertise(peripheral, &server, rx.handle, tx)).await {
        Either::First(never) | Either::Second(never) => never,
    }
}

/// Runs the host: every HCI event and packet goes through here.
async fn run_host(mut runner: Runner<'_, Controller, DefaultPacketPool>) -> ! {
    loop {
        // Returns only on a controller error; running again resets it.
        let _ = runner.run().await;
        Timer::after(Duration::from_secs(1)).await;
    }
}

/// Advertises, serves the central that connects, and advertises again.
async fn advertise(
    mut peripheral: Peripheral<'_, Controller, DefaultPacketPool>,
    server: &Server<'_>,
    rx: u16,
    tx: Characteristic<[u8]>,
) -> ! {
    // The name in the advertisement itself, the service in the scan response:
    // a 128-bit UUID and a long name do not fit 31 bytes together.
    let mut adv = [0u8; 31];
    let adv_len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::CompleteLocalName(DEVICE_NAME.as_bytes()),
        ],
        &mut adv,
    )
    .unwrap();
    let mut scan = [0u8; 31];
    let scan_len = AdStructure::encode_slice(
        &[AdStructure::CompleteServiceUuids128(&[NUS.to_le_bytes()])],
        &mut scan,
    )
    .unwrap();
    let params = AdvertisementParameters {
        interval_min: Duration::from_millis(100),
        interval_max: Duration::from_millis(100),
        ..Default::default()
    };
    loop {
        let data = Advertisement::ConnectableScannableUndirected {
            adv_data: &adv[..adv_len],
            scan_data: &scan[..scan_len],
        };
        let advertiser = match peripheral.advertise(&params, data).await {
            Ok(advertiser) => advertiser,
            Err(_) => {
                // The host is still starting, or starting over.
                Timer::after(Duration::from_secs(1)).await;
                continue;
            }
        };
        // Nobody is connected: what `send` queues meanwhile is dropped.
        let accepted = match select(advertiser.accept(), drop_outgoing()).await {
            Either::First(accepted) => accepted,
            Either::Second(never) => never,
        };
        let Ok(conn) = accepted.and_then(|conn| conn.with_attribute_server(server)) else {
            continue;
        };
        CONNECTED.store(true, Ordering::Relaxed);
        serve(&conn, rx, &tx).await;
        CONNECTED.store(false, Ordering::Relaxed);
    }
}

/// Empties the send queue while no central listens.
async fn drop_outgoing() -> ! {
    loop {
        let _ = OUTGOING.receive().await;
    }
}

/// Serves one central until it disconnects.
async fn serve(conn: &GattConnection<'_, '_, DefaultPacketPool>, rx: u16, tx: &Characteristic<[u8]>) {
    loop {
        match select(conn.next(), OUTGOING.receive()).await {
            Either::First(GattConnectionEvent::Disconnected { .. }) => return,
            Either::First(GattConnectionEvent::Gatt { event }) => {
                match &event {
                    GattEvent::Write(write) if write.handle() == rx => {
                        // One write fits the ATT MTU less 3: MAX_DATA at most.
                        let data = write.with_data(|_offset, data| heapless::Vec::from_slice(data));
                        if let Ok(data) = data {
                            // Dropped when nobody is reading.
                            let _ = INCOMING.try_send(Packet { data });
                        }
                    }
                    _ => {}
                }
                // Answers the central: a write response, a read, discovery.
                if let Ok(reply) = event.accept() {
                    reply.send().await;
                }
            }
            Either::First(_) => {}
            Either::Second(data) => {
                let piece = usize::from(conn.raw().att_mtu()).saturating_sub(3).max(1);
                for chunk in data.chunks(piece) {
                    // Sent only if the central subscribed to TX.
                    if tx.notify(conn, chunk, false).await.is_err() {
                        break;
                    }
                }
            }
        }
    }
}
