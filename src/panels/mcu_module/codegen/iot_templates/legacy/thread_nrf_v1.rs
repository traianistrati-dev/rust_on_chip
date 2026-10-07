// Everything below is editable — your changes are preserved on regeneration.
//
// From anywhere in the firmware:
//
//     pins::configs::thread::wait_attached().await;
//     pins::configs::thread::send_to(addr, port, b"21.5").await.ok();
//     let d = pins::configs::thread::receive().await;   // d.from, d.data
//     let me = pins::configs::thread::addresses();        // link-local, mesh-local, OMR
//
// A Thread end device on OpenThread (openthread 0.4): it joins the network
// THREAD_DATASET in secrets.rs describes - the Active Operational Dataset as
// hex, what `ot-ctl dataset active -x` prints on a border router - and never
// forms one. With the dataset empty, refused, or short of what attaching
// needs (network key, name, channel, PAN ID, extended PAN ID) nothing starts
// and `role()` stays `Disabled`. One UDP socket on UDP_PORT; `send_to` sends
// from it, `receive` returns what arrives on it - a datagram longer than
// MAX_DATA is dropped. A border router routes the mesh to your LAN (an OMR
// address in `addresses()`).
//
// It is a Minimal End Device that stays awake (rx-on-when-idle): it never
// routes for others and never sleeps. The radio is embassy-nrf's 802.15.4
// driver with OpenThread's MAC in software, which acknowledges frames late
// (500-650 us where the standard asks 192 us): unicasts to the board are
// retried a few times before they land, and a sleepy device would not
// attach. OpenThread keeps its settings in RAM: the board joins anew on every
// boot.
//
// It takes RADIO, RNG, the 32 MHz crystal (main.rs starts it) and EGU0_SWI0,
// whose interrupt runs the radio's own executor at priority 7. The project
// builds for thumbv7em-none-eabi - soft-float, the FPU unused - because
// openthread-sys ships OpenThread compiled for it and for no hard-float
// target: no C compiler, CMake or libclang is needed. Any OpenThread feature
// beyond its defaults would bring that C build back.

use core::cell::RefCell;
use core::net::{Ipv6Addr, SocketAddrV6};
use core::sync::atomic::{AtomicU8, Ordering};

use embassy_executor::{InterruptExecutor, Spawner};
use embassy_futures::join::join3;
use embassy_nrf::interrupt;
use embassy_nrf::interrupt::{InterruptExt, Priority};
use embassy_nrf::mode::Blocking;
use embassy_nrf::rng::Rng;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::Timer;
use openthread::nrf::{Ieee802154, NrfRadio};
use openthread::{
    DeviceRole, EmbassyTimeTimer, MacRadio, MacRadioResources, OpenThread, OtResources,
    OtUdpResources, PhyRadioRunner, ProxyRadio, ProxyRadioResources, SimpleRamSettings, UdpSocket,
};
use static_cell::StaticCell;
// OpenThread's C code calls `strcmp` and `strstr`; tinyrlibc provides them.
use tinyrlibc as _;

use super::secrets::THREAD_DATASET;

/// Largest datagram `send_to` takes and `receive` returns; a longer one that
/// arrives is dropped whole, never cut.
pub const MAX_DATA: usize = 512;
/// Most addresses `addresses` reports.
pub const MAX_ADDRS: usize = 6;

const UDP_SOCKETS: usize = 1;
const UDP_RX: usize = 1280;

/// Where the device stands in the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Not started: THREAD_DATASET is empty, incomplete or was refused.
    Disabled,
    /// Looking for a parent.
    Detached,
    /// Attached to a parent router.
    Child,
    Router,
    Leader,
}

/// One datagram `receive` returns.
pub struct Datagram {
    pub from: SocketAddrV6,
    pub data: heapless::Vec<u8, MAX_DATA>,
}

#[derive(Debug)]
pub enum SendError {
    /// More than MAX_DATA bytes.
    TooLarge,
}

static ROLE: AtomicU8 = AtomicU8::new(0);
static ADDRS: Mutex<CriticalSectionRawMutex, RefCell<heapless::Vec<Ipv6Addr, MAX_ADDRS>>> =
    Mutex::new(RefCell::new(heapless::Vec::new()));
static OUTGOING: Channel<CriticalSectionRawMutex, (SocketAddrV6, heapless::Vec<u8, MAX_DATA>), 4> =
    Channel::new();
static INCOMING: Channel<CriticalSectionRawMutex, Datagram, 4> = Channel::new();

/// The device's role right now.
pub fn role() -> Role {
    match ROLE.load(Ordering::Relaxed) {
        1 => Role::Detached,
        2 => Role::Child,
        3 => Role::Router,
        4 => Role::Leader,
        _ => Role::Disabled,
    }
}

/// Attached to the network - `send_to` can reach beyond the board.
pub fn attached() -> bool {
    matches!(role(), Role::Child | Role::Router | Role::Leader)
}

/// Returns once the device is attached. Never, while THREAD_DATASET is empty.
pub async fn wait_attached() {
    while !attached() {
        Timer::after_millis(100).await;
    }
}

/// The device's IPv6 addresses: link-local, mesh-local, and the OMR one a
/// border router hands out.
pub fn addresses() -> heapless::Vec<Ipv6Addr, MAX_ADDRS> {
    ADDRS.lock(|a| a.borrow().clone())
}

/// Queue a UDP datagram to `addr`:`port`, sent from UDP_PORT. Waits only
/// while 4 datagrams are already queued.
pub async fn send_to(addr: Ipv6Addr, port: u16, data: &[u8]) -> Result<(), SendError> {
    let data = heapless::Vec::from_slice(data).map_err(|_| SendError::TooLarge)?;
    OUTGOING.send((SocketAddrV6::new(addr, port, 0, 0), data)).await;
    Ok(())
}

/// The next datagram that arrived on UDP_PORT. 4 are kept; more are dropped
/// while nobody receives.
pub async fn receive() -> Datagram {
    INCOMING.receive().await
}

static EXECUTOR_RADIO: InterruptExecutor = InterruptExecutor::new();

#[interrupt]
unsafe fn EGU0_SWI0() {
    unsafe { EXECUTOR_RADIO.on_interrupt() }
}

/// Bring Thread up. Called once, by `main.rs`.
pub fn start(spawner: Spawner, radio: Ieee802154<'static>, rng: &'static mut Rng<'static, Blocking>) {
    let mut eui64 = [0u8; 8];
    rng.blocking_fill_bytes(&mut eui64);

    static RES: StaticCell<OtResources> = StaticCell::new();
    static UDP: StaticCell<OtUdpResources<UDP_SOCKETS, UDP_RX>> = StaticCell::new();
    static SETTINGS_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
    static SETTINGS: StaticCell<SimpleRamSettings<'static>> = StaticCell::new();
    static PROXY: StaticCell<ProxyRadioResources> = StaticCell::new();

    let settings = SETTINGS.init(SimpleRamSettings::new(SETTINGS_BUF.init([0; 1024])));
    let ot = OpenThread::new_with_udp(eui64, rng, settings, RES.init(OtResources::new()), UDP.init(OtUdpResources::new()))
        .unwrap();

    let (proxy, phy) = ProxyRadio::new(PROXY.init(ProxyRadioResources::new()));
    interrupt::EGU0_SWI0.set_priority(Priority::P7);
    let high = EXECUTOR_RADIO.start(interrupt::EGU0_SWI0);
    high.spawn(radio_task(phy, NrfRadio::new(radio)).unwrap());
    spawner.spawn(ot_task(ot.clone(), proxy).unwrap());

    // An empty dataset is "set" too (it deletes the stored one), and Thread
    // would then look for any parent on defaults: start only on a dataset
    // with everything attaching needs.
    let up = ot.set_active_dataset_tlv_hexstr(THREAD_DATASET).is_ok()
        && ot.is_commissioned()
        && ot.enable_ipv6(true).is_ok()
        && ot.enable_thread(true).is_ok();
    if up {
        spawner.spawn(thread_task(ot).unwrap());
    }
}

#[embassy_executor::task]
async fn ot_task(ot: OpenThread<'static>, radio: ProxyRadio<'static>) -> ! {
    ot.run(radio).await
}

#[embassy_executor::task]
async fn radio_task(mut runner: PhyRadioRunner<'static>, radio: NrfRadio<'static>) -> ! {
    static MAC: StaticCell<MacRadioResources> = StaticCell::new();
    runner
        .run(MacRadio::new(radio, EmbassyTimeTimer, MAC.init(MacRadioResources::new())))
        .await
}

/// The one task that owns the socket: it keeps `role()` and `addresses()`
/// current, and moves datagrams between the socket and the two queues.
#[embassy_executor::task]
async fn thread_task(ot: OpenThread<'static>) -> ! {
    let socket = UdpSocket::bind(ot.clone(), &SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, UDP_PORT, 0, 0)).unwrap();
    let state = async {
        loop {
            let r = match ot.device_role() {
                DeviceRole::Detached => 1,
                DeviceRole::Child => 2,
                DeviceRole::Router => 3,
                DeviceRole::Leader => 4,
                _ => 0,
            };
            ROLE.store(r, Ordering::Relaxed);
            let mut list = heapless::Vec::new();
            let _ = ot.ipv6_addrs(|a| {
                if let Some((addr, _)) = a {
                    let _ = list.push(addr);
                }
                Ok(())
            });
            ADDRS.lock(|a| *a.borrow_mut() = list);
            ot.wait_changed().await;
        }
    };
    let rx = async {
        // As large as the socket takes, so a datagram over MAX_DATA is seen
        // whole - and dropped - rather than cut to fit.
        let mut buf = [0u8; UDP_RX];
        loop {
            if let Ok((len, _local, remote)) = socket.recv(&mut buf).await {
                if let Ok(data) = heapless::Vec::from_slice(&buf[..len]) {
                    let _ = INCOMING.try_send(Datagram { from: remote, data });
                }
            }
        }
    };
    let tx = async {
        loop {
            let (to, data) = OUTGOING.receive().await;
            let _ = socket.send(&data, None, &to).await;
        }
    };
    join3(state, rx, tx).await;
    unreachable!()
}
