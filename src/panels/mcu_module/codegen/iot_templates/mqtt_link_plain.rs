
// Everything below is editable — your changes are preserved on regeneration.
//
// How mqtt.rs reaches the broker: plain TCP on BROKER_PORT. Nothing is
// encrypted - the MQTT password and every message cross the network as they
// are. Switch TLS on in the IoT tab to have this file replaced by one that
// encrypts and checks the broker's certificate.

use embassy_net::tcp::TcpSocket;

/// The TCP receive buffer mqtt.rs gives its socket.
pub const TCP_RX: usize = 1024;

/// What rust-mqtt talks through: the socket itself.
pub type Link<'s> = TcpSocket<'s>;

/// Nothing to keep between connections.
pub struct LinkBuffers;

impl LinkBuffers {
    pub const fn new() -> Self {
        Self
    }
}

impl Default for LinkBuffers {
    fn default() -> Self {
        Self::new()
    }
}

/// Called once, before the first connection.
pub fn init() {}

/// The connected socket, as it is.
pub(super) async fn open<'s>(socket: TcpSocket<'s>, _links: &'s mut LinkBuffers) -> Result<Link<'s>, ()> {
    Ok(socket)
}
