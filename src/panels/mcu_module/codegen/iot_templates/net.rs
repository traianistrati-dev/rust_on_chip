
// Everything below is editable — your changes are preserved on regeneration.

use embassy_net::{Ipv4Address, Ipv4Cidr, StaticConfigV4};

/// The address the stack starts with: a DHCP lease, or the static one above.
pub fn config() -> embassy_net::Config {
    if DHCP {
        return embassy_net::Config::dhcpv4(Default::default());
    }
    let ip = |[a, b, c, d]: [u8; 4]| Ipv4Address::new(a, b, c, d);
    let mut dns_servers = heapless::Vec::new();
    let _ = dns_servers.push(ip(DNS));
    embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(ip(STATIC_IP), PREFIX_LEN),
        gateway: Some(ip(GATEWAY)),
        dns_servers,
    })
}
