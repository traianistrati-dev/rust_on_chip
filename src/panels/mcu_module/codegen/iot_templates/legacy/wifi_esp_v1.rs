
// Everything below is editable — your changes are preserved on regeneration.

use embassy_executor::Spawner;
use embassy_net::{Runner, Stack, StackResources};
use embassy_time::{Duration, Timer};
use esp_radio::wifi::{AuthenticationMethod, Config, Interface, WifiController, sta::StationConfig};
use static_cell::StaticCell;

use super::net;
use super::secrets::{WIFI_PASSWORD, WIFI_SSID};

/// Seconds between two attempts to join the access point.
pub const RETRY_S: u64 = 5;

/// Starts the radio and the IP stack, and the two tasks that keep them up.
///
/// The stack it returns is `Copy`: hand it to whatever opens sockets. It has
/// an address once `stack.wait_config_up().await` returns.
pub fn init(spawner: Spawner, wifi: esp_hal::peripherals::WIFI<'static>) -> Stack<'static> {
    let (controller, interfaces) = esp_radio::wifi::new(wifi, Default::default()).unwrap();
    // The seed only spreads TCP port numbers and sequence numbers.
    let rng = esp_hal::rng::Rng::new();
    let seed = (u64::from(rng.random()) << 32) | u64::from(rng.random());
    static RESOURCES: StaticCell<StackResources<{ net::SOCKETS }>> = StaticCell::new();
    let (stack, runner) = embassy_net::new(
        interfaces.station,
        net::config(),
        RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(connection(controller).unwrap());
    spawner.spawn(net_task(runner).unwrap());
    stack
}

/// Joins the access point, and joins it again whenever the link drops.
#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    let mut station = StationConfig::default()
        .with_ssid(WIFI_SSID)
        .with_password(WIFI_PASSWORD.into());
    if WIFI_PASSWORD.is_empty() {
        // An open network: WPA2 with no password would never join it.
        station = station.with_auth_method(AuthenticationMethod::None);
    }
    let config = Config::Station(station);
    // Fails only on an SSID over 32 bytes or a password over 64.
    controller.set_config(&config).unwrap();
    loop {
        if controller.connect_async().await.is_ok() {
            // Returns once the station is off the network again.
            let _ = controller.wait_for_disconnect_async().await;
        }
        Timer::after(Duration::from_secs(RETRY_S)).await;
    }
}

/// Runs the IP stack: DHCP, ARP, every socket's traffic.
#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface<'static>>) -> ! {
    runner.run().await
}
