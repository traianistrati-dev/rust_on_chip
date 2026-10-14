
// Everything below is editable — your changes are preserved on regeneration.
//
// TLS 1.3 (embedded-tls) for every client the IoT tab generates: mqtt_link.rs
// and http.rs hand `open()` a connected socket and a `Target` - the name the
// server's certificate must carry, whether to check it, the CAs it must end
// in, the board's own certificate - and get the connection back, handshake
// done. Checked with VERIFY: every signature along the chain, that each
// certificate after the server's own is a CA, the name (the first 3 DNS
// names, or the CN; never an IP address), and the dates once wall_clock()
// knows the time (SNTP). Not checked: revocation. Without it the traffic is
// encrypted but nobody is authenticated.
//
// TLS 1.3 only, AES-128-GCM with P-256: a server that speaks only TLS 1.2
// cannot be reached. Every certificate in a chain must be signed with RSA or
// ECDSA P-256 - or P-384 with the tab's "P-384 chains" (embedded-tls's
// `p384`, about 114 KiB of flash more). Checking RSA takes the heap, about
// 22 KB at its peak: one handshake at a time, so two never need it at once.

use core::net::Ipv4Addr;

use embassy_net::tcp::{Error as TcpError, State, TcpSocket};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Timer;
use embedded_io_async::{ErrorType, Read, Write};
use embedded_tls::{
    Aes128GcmSha256, Certificate, CryptoProvider, CryptoRngCore, SignatureScheme, TlsConfig,
    TlsConnection, TlsContext, TlsError, TlsVerifier,
};

// ── This chip's pieces ──
type TlsRng = esp_hal::rng::Trng;
/// The hardware TRNG - Wi-Fi is up by now, which keeps its entropy source on.
fn tls_rng() -> Result<TlsRng, ()> {
    esp_hal::rng::Trng::try_new().map_err(drop)
}
/// Called before a handshake: the heap is main.rs's.
pub fn init() {}
// ── end of this chip's pieces ──
