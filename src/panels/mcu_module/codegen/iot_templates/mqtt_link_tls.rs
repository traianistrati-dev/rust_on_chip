
// Everything below is editable — your changes are preserved on regeneration.
//
// How mqtt.rs reaches the broker: TLS 1.3 over TCP on BROKER_PORT. With
// VERIFY the broker must present a chain that ends in a CA of mqtt_ca.rs and
// names SERVER_NAME, and prove it holds that certificate's key - or the
// connection is refused, and tried again RETRY_S later. With a client
// certificate (CLIENT, from secrets.rs) the board proves who it is when the
// broker asks - AWS IoT does. The handshake and every check are tls.rs's,
// which http.rs shares.

use embassy_net::tcp::TcpSocket;

pub use super::tls::{Link, LinkBuffers, TCP_RX};

/// Called once, before the first connection (on a Pico: TLS's heap).
pub fn init() {
    super::tls::init();
}

/// The connected socket, wrapped in TLS: the handshake, awaited whole.
pub(super) async fn open<'s>(socket: TcpSocket<'s>, links: &'s mut LinkBuffers) -> Result<Link<'s>, ()> {
    let to = super::tls::Target {
        server_name: SERVER_NAME,
        verify: VERIFY,
        cas: &CA_DER,
        client: CLIENT,
        alpn: ALPN,
    };
    super::tls::open(socket, links, &to).await
}
