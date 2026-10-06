
// Everything below is editable — your changes are preserved on regeneration.
//
// From anywhere in the firmware:
//
//     pins::configs::sntp::wait_synced().await;
//     let now = pins::configs::sntp::now_unix();   // Some(seconds since 1970, UTC)
//
// One task asks the server and keeps its answer as (Unix time, `Instant`);
// the time in between comes from the chip's own timer, so reading the clock
// never waits and never touches the network. `SERVER` is a host name or an
// IPv4 address. SNTP is plain UDP: nothing is authenticated.

use core::cell::RefCell;
use core::future::poll_fn;
use core::task::Poll;

use embassy_executor::Spawner;
use embassy_net::dns::DnsQueryType;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpEndpoint, Stack};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::waitqueue::MultiWakerRegistration;
use embassy_time::{Duration, Instant, Timer, with_timeout};

/// Seconds to wait for the server's answer.
pub const TIMEOUT_S: u64 = 5;
/// Seconds before asking again after an attempt that got no usable answer.
pub const RETRY_S: u64 = 10;
/// Tasks that can wait in `wait_synced` at once. More still works, but they
/// then keep waking one another until the clock is set.
pub const WAITERS: usize = 4;

/// NTP counts seconds from 1900, Unix from 1970.
const NTP_TO_UNIX_S: u32 = 2_208_988_800;

struct Clock {
    /// Unix time in microseconds at that `Instant`, from the last good answer.
    synced: Option<(u64, Instant)>,
    /// The tasks waiting in `wait_synced`.
    waiters: MultiWakerRegistration<WAITERS>,
}

// A critical section, not atomics: RP2040 and ESP32-C3 have no 64-bit ones.
static CLOCK: Mutex<CriticalSectionRawMutex, RefCell<Clock>> = Mutex::new(RefCell::new(Clock {
    synced: None,
    waiters: MultiWakerRegistration::new(),
}));

/// Milliseconds since 1970-01-01 UTC; `None` until the first answer.
///
/// A resync can set it back by what the chip's crystal gained since the last
/// one (tens of ms an hour): time intervals with `embassy_time::Instant`.
pub fn now_unix_ms() -> Option<u64> {
    let (unix_us, at) = CLOCK.lock(|c| c.borrow().synced)?;
    Some((unix_us + at.elapsed().as_micros()) / 1000)
}

/// Seconds since 1970-01-01 UTC; `None` until the first answer.
pub fn now_unix() -> Option<u64> {
    now_unix_ms().map(|ms| ms / 1000)
}

/// Returns once the clock is set - at once if it already is.
pub async fn wait_synced() {
    poll_fn(|cx| {
        CLOCK.lock(|c| {
            let mut c = c.borrow_mut();
            if c.synced.is_some() {
                Poll::Ready(())
            } else {
                c.waiters.register(cx.waker());
                Poll::Pending
            }
        })
    })
    .await
}

/// Starts the task that keeps the clock.
pub fn start(spawner: Spawner, stack: Stack<'static>) {
    spawner.spawn(sntp_task(stack).unwrap());
}

#[embassy_executor::task]
async fn sntp_task(stack: Stack<'static>) -> ! {
    loop {
        stack.wait_config_up().await;
        let pause = match sync(stack).await {
            Ok(()) => INTERVAL_S,
            Err(()) => RETRY_S,
        };
        Timer::after(Duration::from_secs(pause)).await;
    }
}

/// One question to the server. `Ok` once it has answered: with the time,
/// which sets the clock, or with a "kiss-o'-death", which asks not to be
/// asked again so soon.
async fn sync(stack: Stack<'static>) -> Result<(), ()> {
    // `dns_query` hands an IPv4 literal back as it is.
    let found = stack
        .dns_query(SERVER, DnsQueryType::A)
        .await
        .map_err(drop)?;
    let server = IpEndpoint::new(*found.first().ok_or(())?, PORT);

    let mut rx_meta = [PacketMetadata::EMPTY; 2];
    let mut rx_buf = [0u8; 128];
    let mut tx_meta = [PacketMetadata::EMPTY; 1];
    let mut tx_buf = [0u8; 48];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    // Port 0 = a fresh local port: a late answer to an earlier try cannot land here.
    socket.bind(0).map_err(drop)?;

    // Version 4, mode 3 (client). The transmit timestamp is only a tag: the
    // server copies it back, and an answer without it is not ours.
    let sent = Instant::now();
    let tag = sent.as_ticks();
    let mut request = [0u8; 48];
    request[0] = 0x23;
    request[40..48].copy_from_slice(&tag.to_be_bytes());
    socket.send_to(&request, server).await.map_err(drop)?;

    let mut reply = [0u8; 128];
    let timeout = Duration::from_secs(TIMEOUT_S);
    let (len, from) = with_timeout(timeout, socket.recv_from(&mut reply))
        .await
        .map_err(drop)?
        .map_err(drop)?;
    let received = Instant::now();

    // Mode 4 = a server's answer; the tag says it answers this request.
    let mode = reply[0] & 0x07;
    if from.endpoint != server || len < 48 || mode != 4 || be64(&reply[24..32]) != tag {
        return Err(());
    }
    // Stratum 0 = "kiss-o'-death": the server asks to be left alone. That
    // counts as an answer, so the next question waits `INTERVAL_S`.
    if reply[1] == 0 {
        return Ok(());
    }
    let server_rx = be64(&reply[32..40]);
    let server_tx = be64(&reply[40..48]);
    // How long the server held the request (32.32 fixed point): a second or
    // more is not a real answer. Leap indicator 3 = its own clock is not set.
    let held = server_tx.wrapping_sub(server_rx);
    if reply[0] >> 6 == 3 || server_tx == 0 || held >> 32 != 0 {
        return Err(());
    }
    // The round trip minus the server's share; half of it is how old
    // `server_tx` already was when it arrived.
    let held_us = (held * 1_000_000) >> 32;
    let delay_us = received
        .duration_since(sent)
        .as_micros()
        .saturating_sub(held_us);
    // NTP seconds wrap in 2036; the wrapping subtraction stays right until 2106.
    let secs = u64::from(((server_tx >> 32) as u32).wrapping_sub(NTP_TO_UNIX_S));
    let frac_us = ((server_tx & 0xFFFF_FFFF) * 1_000_000) >> 32;
    let unix_us = secs * 1_000_000 + frac_us + delay_us / 2;

    CLOCK.lock(|c| {
        let mut c = c.borrow_mut();
        c.synced = Some((unix_us, received));
        c.waiters.wake();
    });
    Ok(())
}

/// The big-endian `u64` in an 8-byte field.
fn be64(field: &[u8]) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(field);
    u64::from_be_bytes(bytes)
}
