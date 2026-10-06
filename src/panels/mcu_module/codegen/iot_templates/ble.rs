
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

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
// trouble-host 0.6 locks its tables with embassy-sync 0.7, a crate apart
// from the 0.8 above: its mutex has to come from there.
use embassy_sync_0_7::blocking_mutex::raw::NoopRawMutex;
use embassy_time::{Duration, Timer};
use trouble_host::prelude::{
    AdStructure, Address, Advertisement, AdvertisementParameters, AttributeServer, AttributeTable,
    BR_EDR_NOT_SUPPORTED, Characteristic, CharacteristicProp, DefaultPacketPool,
    ExternalController, GapConfig, GattConnection, GattConnectionEvent, GattEvent, Host,
    HostResources, LE_GENERAL_DISCOVERABLE, Peripheral, Runner, Service,
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
/// HCI commands that may wait for the controller at once.
const SLOTS: usize = 20;

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

type Controller = ExternalController<Radio, SLOTS>;
type Server<'v> = AttributeServer<'v, NoopRawMutex, DefaultPacketPool, ATTRIBUTES, 1, CONNECTIONS>;

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

/// Starts the task that owns Bluetooth, on the radio `main.rs` switched on.
///
/// `mac`: `None` keeps the radio's own (public) address; `Some` gives the
/// board a random static address made of it - the same on every boot and
/// different per board - for a radio whose own address is not its alone.
pub fn start(spawner: Spawner, radio: Radio, mac: Option<[u8; 6]>) {
    spawner.spawn(ble_task(ExternalController::new(radio), mac.map(random_static)).unwrap());
}

/// A random static address made of `mac`. Bluetooth sends an address least
/// significant byte first, and a random static one has its top two bits set.
fn random_static(mac: [u8; 6]) -> Address {
    let mut bytes = mac;
    bytes.reverse();
    bytes[5] |= 0xC0;
    Address::random(bytes)
}

#[embassy_executor::task]
async fn ble_task(controller: Controller, address: Option<Address>) -> ! {
    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS, CHANNELS> =
        HostResources::new();
    let stack = trouble_host::new(controller, &mut resources);
    let stack = match address {
        Some(address) => stack.set_random_address(address),
        None => stack,
    };
    let Host {
        peripheral, runner, ..
    } = stack.build();

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
    let scan_len =
        AdStructure::encode_slice(&[AdStructure::ServiceUuids128(&[NUS.to_le_bytes()])], &mut scan)
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
                        if let Ok(data) = heapless::Vec::from_slice(write.data()) {
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
                    if tx.notify(conn, chunk).await.is_err() {
                        break;
                    }
                }
            }
        }
    }
}
