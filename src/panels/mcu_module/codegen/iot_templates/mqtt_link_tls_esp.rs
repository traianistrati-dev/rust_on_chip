
// Everything below is editable — your changes are preserved on regeneration.
//
// How mqtt.rs reaches the broker: TLS 1.3 (embedded-tls) over TCP on
// BROKER_PORT. With VERIFY the broker must present a chain that ends in the CA
// of mqtt_ca.rs and names SERVER_NAME, and prove it holds that certificate's
// key - or the connection is refused, and tried again RETRY_S later.
// Checked: every signature along the chain, that each certificate after the
// broker's own is a CA, the name (the first 3 DNS names, or the CN; never an
// IP address), and the dates once wall_clock() knows the time (SNTP). Not
// checked: revocation. Without VERIFY the traffic is encrypted but nobody is
// authenticated: whoever controls the network can pose as the broker and read
// the MQTT password.
//
// TLS 1.3 only, AES-128-GCM with P-256: a broker that speaks only TLS 1.2
// (broker.hivemq.com) cannot be reached. Every certificate in the broker's
// chain must be signed with RSA or ECDSA P-256 - or P-384 with the tab's
// "P-384 chains" (embedded-tls's `p384`, about 114 KiB of flash more), which
// an ECDSA Let's Encrypt certificate (certbot's default) needs: its issuers
// are P-384. An RSA one (`--key-type rsa`) works without. Checking RSA takes
// the heap, about 22 KB at its peak.

use core::net::Ipv4Addr;

use embassy_net::tcp::{Error as TcpError, State, TcpSocket};
use embassy_time::Timer;
use embedded_io_async::{ErrorType, Read, Write};
use embedded_tls::{
    Aes128GcmSha256, CryptoProvider, CryptoRngCore, TlsConfig, TlsConnection, TlsContext, TlsError,
    TlsVerifier, UnsecureProvider,
};

use super::mqtt_ca::CA_PEM;

/// The TCP receive buffer mqtt.rs gives its socket: a whole TLS record of one
/// MQTT packet (1024 + 22 bytes) must fit, so `Gate` can hold it back until
/// all of it has arrived.
pub const TCP_RX: usize = 2048;
/// TLS records: up to 16 KiB of plaintext and its overhead in; packets out.
const RECORD_RX: usize = 16_640;
const RECORD_TX: usize = 2048;
/// The longest certificate chain the broker may send.
const MAX_CHAIN: usize = 8192;

/// What rust-mqtt talks through: TLS over the socket.
pub type Link<'s> = TlsConnection<'s, Gate<'s>, Aes128GcmSha256>;

/// The TLS record buffers, kept between connections.
pub struct LinkBuffers {
    rx: [u8; RECORD_RX],
    tx: [u8; RECORD_TX],
}

impl LinkBuffers {
    pub const fn new() -> Self {
        Self {
            rx: [0; RECORD_RX],
            tx: [0; RECORD_TX],
        }
    }
}

impl Default for LinkBuffers {
    fn default() -> Self {
        Self::new()
    }
}

// ── This chip's pieces ──
type TlsRng = esp_hal::rng::Trng;
/// The hardware TRNG - Wi-Fi is up by now, which keeps its entropy source on.
fn tls_rng() -> Result<TlsRng, ()> {
    esp_hal::rng::Trng::try_new().map_err(drop)
}
/// Called once, before the first connection: the heap is main.rs's.
pub fn init() {}
// ── end of this chip's pieces ──

/// The certificates' dates are checked against `wall_clock()`.
struct CertClock;

impl embedded_tls::TlsClock for CertClock {
    fn now() -> Option<u64> {
        wall_clock()
    }
}

/// The CA as DER, decoded by the compiler: a broken paste fails the build.
static CA_DER: [u8; der_len(CA_PEM)] = pem_to_der(CA_PEM);

/// The connected socket, wrapped in TLS: the handshake, awaited whole.
pub(super) async fn open<'s>(socket: TcpSocket<'s>, links: &'s mut LinkBuffers) -> Result<Link<'s>, ()> {
    clock_ready().await;
    let mut link: Link<'s> = TlsConnection::new(Gate::new(socket), &mut links.rx, &mut links.tx);
    // A certificate names DNS names, and an address is never sent as SNI.
    let ip = SERVER_NAME.parse::<Ipv4Addr>().is_ok();
    let config = if ip {
        TlsConfig::new()
    } else {
        TlsConfig::new().with_server_name(SERVER_NAME)
    };
    if VERIFY {
        if ip {
            return Err(());
        }
        let mut verifier = Verifier::new(&CA_DER[..]);
        let provider = Provider {
            rng: tls_rng()?,
            verifier: &mut verifier,
        };
        link.open(TlsContext::new(&config, provider))
            .await
            .map_err(drop)?;
        // embedded-tls 0.19 also completes a handshake in which the server sent
        // no certificate at all (its issue #215): then nobody was authenticated.
        if !verifier.authenticated {
            return Err(());
        }
    } else {
        let provider = UnsecureProvider::new::<Aes128GcmSha256>(tls_rng()?);
        link.open(TlsContext::new(&config, provider))
            .await
            .map_err(drop)?;
    }
    Ok(link)
}

/// What embedded-tls asks for during the handshake: randomness, and the
/// verifier that checks the broker's chain, its signature and its name.
struct Provider<'v, 'a> {
    rng: TlsRng,
    verifier: &'v mut Verifier<'a>,
}

impl CryptoProvider for Provider<'_, '_> {
    type CipherSuite = Aes128GcmSha256;
    // No client certificate, so nothing is ever signed.
    type Signature = [u8; 0];

    fn rng(&mut self) -> impl CryptoRngCore {
        &mut self.rng
    }

    fn verifier(&mut self) -> Result<&mut impl TlsVerifier<Aes128GcmSha256>, TlsError> {
        Ok(&mut *self.verifier)
    }
}

/// The TCP socket as embedded-tls sees it. A TLS record is handed over only
/// once all of it has arrived, so when mqtt.rs's `select3` drops a read the
/// TLS layer has never consumed half a record: embedded-tls keeps the 5-byte
/// record header inside the read future, and a record cut there would
/// desynchronise the stream for good.
pub struct Gate<'s> {
    socket: TcpSocket<'s>,
    head: [u8; 5],
    /// Header bytes taken from the socket.
    have: usize,
    /// Header bytes handed to TLS.
    given: usize,
    /// Body bytes of the current record still to hand over.
    body: usize,
}

impl<'s> Gate<'s> {
    fn new(socket: TcpSocket<'s>) -> Self {
        Self {
            socket,
            head: [0; 5],
            have: 0,
            given: 0,
            body: 0,
        }
    }

    fn next_record(&mut self) {
        self.have = 0;
        self.given = 0;
    }
}

impl ErrorType for Gate<'_> {
    type Error = TcpError;
}

impl Read for Gate<'_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, TcpError> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.given == 5 {
            // The body: already queued whole, so this returns at once.
            let want = buf.len().min(self.body);
            let n = self.socket.read(&mut buf[..want]).await?;
            self.body -= n;
            if self.body == 0 {
                self.next_record();
            }
            return Ok(n);
        }
        // A record boundary - the only place a read may wait. Everything it
        // has read so far is kept in `self`, so being dropped loses nothing.
        while self.have < 5 {
            let n = self.socket.read(&mut self.head[self.have..]).await?;
            if n == 0 {
                return Ok(0);
            }
            self.have += n;
        }
        if self.given == 0 {
            let len = usize::from(u16::from_be_bytes([self.head[3], self.head[4]]));
            // An encrypted record carries at least its inner content type and a
            // 16-byte tag (RFC 8446 5.2). A shorter one decrypts to nothing, and
            // embedded-tls 0.19 panics on that (connection.rs:62) - before any
            // certificate is checked, so anyone on the path could send it.
            if self.head[0] == 0x17 && len < 17 {
                return Err(TcpError::ConnectionReset);
            }
            // A record bigger than the socket's buffer can never be queued
            // whole: it streams through (only the handshake's, which is never
            // dropped halfway). Nor once the broker can send nothing more - a
            // closed socket keeps what it holds, and `may_recv` stays true then.
            while len <= self.socket.recv_capacity()
                && self.socket.recv_queue() < len
                && matches!(self.socket.state(), State::Established | State::FinWait1 | State::FinWait2)
            {
                Timer::after_millis(2).await;
            }
            self.body = len;
        }
        let n = buf.len().min(5 - self.given);
        buf[..n].copy_from_slice(&self.head[self.given..self.given + n]);
        self.given += n;
        if self.given == 5 && self.body == 0 {
            self.next_record();
        }
        Ok(n)
    }
}

impl Write for Gate<'_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, TcpError> {
        self.socket.write(buf).await
    }

    async fn flush(&mut self) -> Result<(), TcpError> {
        self.socket.flush().await
    }
}

