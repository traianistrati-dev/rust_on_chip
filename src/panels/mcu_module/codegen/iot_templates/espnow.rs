
// Everything below is editable — your changes are preserved on regeneration.
//
// From anywhere in the firmware:
//
//     pins::configs::espnow::send(pins::configs::espnow::BROADCAST, b"21.5").await.ok();
//     let frame = pins::configs::espnow::receive().await;   // frame.from, frame.data
//
// One task owns ESP-NOW: `send` only queues the frame for it. Frames are not
// encrypted, and every board of the group must be on the same Wi-Fi channel -
// the access point's while the station is on, `CHANNEL` otherwise. While the
// station is joining (or joining again) it scans other channels, and frames
// sent or received meanwhile are lost.
//
// The address another board needs in its `PEERS` is this board's `own_mac()`:
// print it once at boot, or read the `MAC:` line espflash shows when flashing.
//
// A simple flooding mesh on top of this: start every payload with the origin
// board's MAC, a sequence number and a hop count. Whoever calls `receive()`
// drops a frame whose (origin, sequence) it saw recently - a small ring of the
// last few is enough - and otherwise uses it and, while the hop count is above
// zero, sends it again to `BROADCAST` with the count one lower. Every board
// repeats each message once, so it crosses the group without any routes.

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use esp_hal::efuse::{InterfaceMacAddress, interface_mac_address};
use esp_radio::esp_now::{
    BROADCAST_ADDRESS, ESP_NOW_MAX_DATA_LEN, EspNow, EspNowManager, EspNowReceiver, EspNowSender,
    EspNowWifiInterface, PeerInfo,
};
use esp_radio::wifi::{SecondaryChannel, WifiController};

/// Every board on the channel hears a frame sent here; it needs no peer.
pub const BROADCAST: [u8; 6] = BROADCAST_ADDRESS;
/// Largest payload of one frame.
pub const MAX_DATA: usize = ESP_NOW_MAX_DATA_LEN;

/// One frame another board sent here (or to `BROADCAST`).
pub struct Frame {
    /// The board that sent it.
    pub from: [u8; 6],
    pub data: heapless::Vec<u8, MAX_DATA>,
}

/// Why `send` refused a frame.
#[derive(Debug)]
pub enum SendError {
    /// More than `MAX_DATA` bytes.
    TooLarge,
}

/// A frame on its way to the task.
struct Outgoing {
    to: [u8; 6],
    data: heapless::Vec<u8, MAX_DATA>,
}

static OUTGOING: Channel<CriticalSectionRawMutex, Outgoing, 4> = Channel::new();
static INCOMING: Channel<CriticalSectionRawMutex, Frame, 4> = Channel::new();

/// Queues `data` for `to` - a board, or `BROADCAST`. Waits only while the
/// queue is full; whether the frame arrived is not reported.
pub async fn send(to: [u8; 6], data: &[u8]) -> Result<(), SendError> {
    let data = heapless::Vec::from_slice(data).map_err(|_| SendError::TooLarge)?;
    OUTGOING.send(Outgoing { to, data }).await;
    Ok(())
}

/// The next frame another board sent.
pub async fn receive() -> Frame {
    INCOMING.receive().await
}

/// This board's address: the one the others put in their `PEERS`.
pub fn own_mac() -> [u8; 6] {
    let mac = interface_mac_address(InterfaceMacAddress::Station);
    let mut out = [0u8; 6];
    out.copy_from_slice(mac.as_bytes());
    out
}

/// Starts the task that owns ESP-NOW, with every board of `PEERS` on its list.
pub fn start(spawner: Spawner, esp_now: EspNow<'static>) {
    let (manager, sender, receiver) = esp_now.split();
    for peer in PEERS {
        add_peer(&manager, *peer);
    }
    spawner.spawn(esp_now_task(manager, sender, receiver).unwrap());
}

/// Keeps the radio on for ESP-NOW alone, with the Wi-Fi station off.
///
/// `esp_radio::wifi::new` has already started the radio as a station that
/// joins nothing; this moves it to `CHANNEL` and keeps `controller` alive -
/// dropping it would stop Wi-Fi, and ESP-NOW with it.
pub fn hold_radio(spawner: Spawner, mut controller: WifiController<'static>) {
    // Fails only on a channel the country rules (CN: 1..=13) do not allow.
    controller.set_channel(CHANNEL, SecondaryChannel::None).unwrap();
    spawner.spawn(radio_task(controller).unwrap());
}

/// Owns the controller for good, so the radio stays on.
#[embassy_executor::task]
async fn radio_task(_controller: WifiController<'static>) {
    core::future::pending::<()>().await
}

/// Puts `peer` on the list `esp_now_send` checks; already there is fine.
fn add_peer(manager: &EspNowManager<'static>, peer: [u8; 6]) {
    if manager.peer_exists(&peer) {
        return;
    }
    // Channel `None` is whichever the radio is on. Fails only on a full list,
    // and a send to that board then fails too.
    let _ = manager.add_peer(PeerInfo {
        interface: EspNowWifiInterface::Station,
        peer_address: peer,
        lmk: None,
        channel: None,
        encrypt: false,
    });
}

#[embassy_executor::task]
async fn esp_now_task(
    manager: EspNowManager<'static>,
    mut sender: EspNowSender<'static>,
    mut receiver: EspNowReceiver<'static>,
) -> ! {
    loop {
        match select(OUTGOING.receive(), receiver.receive_async()).await {
            Either::First(frame) => {
                add_peer(&manager, frame.to);
                // Awaited to the end, never raced: a send dropped half way
                // leaves its result to the next one. `Err` on a unicast frame
                // means no board acknowledged it.
                let _ = sender.send_async(&frame.to, &frame.data).await;
            }
            Either::Second(received) => {
                // Longer than MAX_DATA: an ESP-NOW v2 sender's frame - dropped.
                if let Ok(data) = heapless::Vec::from_slice(received.data()) {
                    let frame = Frame {
                        from: received.info.src_address,
                        data,
                    };
                    // Dropped when nobody is reading.
                    let _ = INCOMING.try_send(frame);
                }
            }
        }
    }
}
