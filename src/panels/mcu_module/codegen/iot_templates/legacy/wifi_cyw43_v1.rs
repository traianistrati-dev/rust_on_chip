
// Everything below is editable — your changes are preserved on regeneration.

use cyw43::JoinOptions;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_net::{Runner, Stack, StackResources};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use static_cell::StaticCell;

use super::net;
use super::secrets::{WIFI_PASSWORD, WIFI_SSID};

/// Seconds between two attempts to join the access point.
pub const RETRY_S: u64 = 5;

static LED: Signal<CriticalSectionRawMutex, bool> = Signal::new();

/// Switches the on-board LED. It is GPIO0 of the radio, and the radio's
/// `Control` belongs to the Wi-Fi task, so the task does the switching.
pub fn set_led(on: bool) {
    LED.signal(on);
}

/// Starts the IP stack on the radio, and the two tasks that keep it up.
///
/// The stack it returns is `Copy`: hand it to whatever opens sockets. It has
/// an address once `stack.wait_config_up().await` returns.
pub fn init(
    spawner: Spawner,
    device: cyw43::NetDriver<'static>,
    control: cyw43::Control<'static>,
) -> Stack<'static> {
    // The seed only spreads TCP port numbers and sequence numbers.
    let seed = embassy_rp::clocks::RoscRng.next_u64();
    static RESOURCES: StaticCell<StackResources<{ net::SOCKETS }>> = StaticCell::new();
    let (stack, runner) = embassy_net::new(
        device,
        net::config(),
        RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(connection(control, stack).unwrap());
    stack
}

/// Joins the access point, joins it again whenever the link drops, and
/// switches the LED in between.
#[embassy_executor::task]
async fn connection(mut control: cyw43::Control<'static>, stack: Stack<'static>) -> ! {
    loop {
        let options = if WIFI_PASSWORD.is_empty() {
            JoinOptions::new_open()
        } else {
            JoinOptions::new(WIFI_PASSWORD.as_bytes())
        };
        if control.join(WIFI_SSID, options).await.is_ok() {
            // The runner raises the link a moment after the join returns.
            stack.wait_link_up().await;
            // Joined: serve the LED until the link goes down.
            loop {
                match select(LED.wait(), stack.wait_link_down()).await {
                    Either::First(on) => control.gpio_set(0, on).await,
                    Either::Second(()) => break,
                }
            }
        }
        // Before the next attempt - and the LED still answers meanwhile.
        let mut retry = Timer::after(Duration::from_secs(RETRY_S));
        loop {
            match select(LED.wait(), &mut retry).await {
                Either::First(on) => control.gpio_set(0, on).await,
                Either::Second(()) => break,
            }
        }
    }
}

/// Runs the IP stack: DHCP, ARP, every socket's traffic.
#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, cyw43::NetDriver<'static>>) -> ! {
    runner.run().await
}
