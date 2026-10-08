
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
// the heap, about 22 KB at its peak - on a
// Pico this file brings that heap (32 KiB).

use core::net::Ipv4Addr;

use embassy_net::tcp::{Error as TcpError, State, TcpSocket};
use embassy_time::Timer;
use embedded_io_async::{ErrorType, Read, Write};
use embedded_tls::pki::CertVerifier;
use embedded_tls::{
    Aes128GcmSha256, Certificate, CertificateEntryRef, CertificateRef, CertificateVerifyRef,
    CryptoProvider, CryptoRngCore, Sha256, SignatureScheme, TlsConfig, TlsConnection, TlsContext,
    TlsError, TlsVerifier, UnsecureProvider,
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
/// The ring oscillator's random bit - every RP has it; the RP2350's own TRNG
/// is not used yet - whitened: each 32 bytes out are the SHA-256 of 64 raw
/// bytes and a counter.
struct TlsRng {
    counter: u64,
}

impl rand_core_06::RngCore for TlsRng {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        self.fill_bytes(&mut b);
        u32::from_le_bytes(b)
    }

    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.fill_bytes(&mut b);
        u64::from_le_bytes(b)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        use sha2::Digest as _;
        for chunk in dest.chunks_mut(32) {
            let mut raw = [0u8; 64];
            embassy_rp::clocks::RoscRng.fill_bytes(&mut raw);
            let mut hash = sha2::Sha256::new();
            hash.update(raw);
            hash.update(self.counter.to_le_bytes());
            self.counter = self.counter.wrapping_add(1);
            let out = hash.finalize();
            chunk.copy_from_slice(&out[..chunk.len()]);
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core_06::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl rand_core_06::CryptoRng for TlsRng {}

fn tls_rng() -> Result<TlsRng, ()> {
    Ok(TlsRng { counter: 0 })
}

/// RSA (num-bigint) allocates, and the Pico has no heap of its own: TLS
/// brings one. Only the handshake's certificate checks use it (about 22 KB
/// at their peak, verifying a Let's Encrypt chain).
#[global_allocator]
static HEAP: embedded_alloc::LlffHeap = embedded_alloc::LlffHeap::empty();
const HEAP_SIZE: usize = 32 * 1024;

/// Called once, before the first connection: gives the heap its memory.
pub fn init() {
    static mut MEMORY: [u8; HEAP_SIZE] = [0; HEAP_SIZE];
    // SAFETY: called once, before the first allocation; nothing else
    // touches MEMORY.
    unsafe { HEAP.init(&raw mut MEMORY as usize, HEAP_SIZE) }
}
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

/// embedded-tls's `CertVerifier` (each certificate's signature by the next,
/// the last by CA_DER, the name, the dates when CertClock knows the time),
/// plus the checks it leaves out: every certificate after the broker's own
/// must be a CA - without it, a chain [any name <- an ordinary certificate
/// someone bought <- ... <- CA_DER] is accepted - and an RSA-PSS signature
/// only from an RSA key: embedded-tls 0.19 unwraps the key as RSA (pki.rs:214),
/// and a replayed public EC chain would panic it there.
struct Verifier<'a> {
    inner: CertVerifier<'a, Aes128GcmSha256, CertClock, MAX_CHAIN>,
    /// The broker's chain passed.
    chain_ok: bool,
    /// ... its own certificate carries an RSA key ...
    leaf_rsa: bool,
    /// ... and it signed this handshake with that certificate's key.
    authenticated: bool,
}

impl<'a> Verifier<'a> {
    fn new(ca: &'a [u8]) -> Self {
        Self {
            inner: CertVerifier::new(Certificate::X509(ca)),
            chain_ok: false,
            leaf_rsa: false,
            authenticated: false,
        }
    }
}

impl TlsVerifier<Aes128GcmSha256> for Verifier<'_> {
    fn set_hostname_verification(&mut self, hostname: &str) -> Result<(), TlsError> {
        self.inner.set_hostname_verification(hostname)
    }

    fn verify_certificate(&mut self, transcript: &Sha256, chain: CertificateRef) -> Result<(), TlsError> {
        for issuer in chain.entries.iter().skip(1) {
            match issuer {
                CertificateEntryRef::X509(der) if is_ca(der) => {}
                _ => return Err(TlsError::InvalidCertificate),
            }
        }
        let leaf_rsa = matches!(chain.entries.first(), Some(CertificateEntryRef::X509(der)) if key_is_rsa(der));
        self.inner.verify_certificate(transcript, chain)?;
        self.leaf_rsa = leaf_rsa;
        self.chain_ok = true;
        Ok(())
    }

    fn verify_signature(&mut self, verify: CertificateVerifyRef) -> Result<(), TlsError> {
        // Without a checked chain first, embedded-tls 0.19 would panic here.
        if !self.chain_ok {
            return Err(TlsError::InvalidCertificate);
        }
        let pss = matches!(
            verify.signature_scheme,
            SignatureScheme::RsaPssRsaeSha256 | SignatureScheme::RsaPssRsaeSha384 | SignatureScheme::RsaPssRsaeSha512
        );
        if pss && !self.leaf_rsa {
            return Err(TlsError::InvalidSignatureScheme);
        }
        self.inner.verify_signature(verify)?;
        self.authenticated = true;
        Ok(())
    }
}

/// One DER element at the start of `d`: (tag, contents, what follows).
fn tlv(d: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, d) = d.split_first()?;
    let (&first, mut d) = d.split_first()?;
    let mut len = usize::from(first);
    if first >= 0x80 {
        let n = usize::from(first & 0x7F);
        if n == 0 || n > 3 || d.len() < n {
            return None;
        }
        len = d[..n].iter().fold(0, |l, &b| (l << 8) | usize::from(b));
        d = &d[n..];
    }
    (d.len() >= len).then(|| (tag, &d[..len], &d[len..]))
}

/// `true` when the certificate's basicConstraints say cA = TRUE.
fn is_ca(cert: &[u8]) -> bool {
    let Some((0x30, cert, _)) = tlv(cert) else { return false };
    let Some((0x30, mut tbs, _)) = tlv(cert) else { return false };
    // The extensions are TBSCertificate's [3]; skip everything before them.
    while let Some((tag, value, rest)) = tlv(tbs) {
        tbs = rest;
        if tag != 0xA3 {
            continue;
        }
        let Some((0x30, mut extensions, _)) = tlv(value) else { return false };
        while let Some((0x30, extension, rest)) = tlv(extensions) {
            extensions = rest;
            let Some((0x06, oid, mut extension)) = tlv(extension) else { return false };
            if oid != [0x55, 0x1D, 0x13] {
                continue; // not basicConstraints (2.5.29.19)
            }
            if let Some((0x01, _, rest)) = tlv(extension) {
                extension = rest; // the `critical` flag
            }
            let Some((0x04, value, _)) = tlv(extension) else { return false };
            let Some((0x30, constraints, _)) = tlv(value) else { return false };
            return matches!(tlv(constraints), Some((0x01, [0xFF], _)));
        }
        return false;
    }
    false
}

/// `true` when the certificate's key is RSA (rsaEncryption, 1.2.840.113549.1.1.1).
fn key_is_rsa(cert: &[u8]) -> bool {
    let Some((0x30, cert, _)) = tlv(cert) else { return false };
    let Some((0x30, mut tbs, _)) = tlv(cert) else { return false };
    if let Some((0xA0, _, rest)) = tlv(tbs) {
        tbs = rest; // the version
    }
    // serial, signature, issuer, validity, subject - then the key
    for _ in 0..5 {
        let Some((_, _, rest)) = tlv(tbs) else { return false };
        tbs = rest;
    }
    let Some((0x30, key, _)) = tlv(tbs) else { return false };
    let Some((0x30, algorithm, _)) = tlv(key) else { return false };
    matches!(
        tlv(algorithm),
        Some((0x06, [0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01], _))
    )
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

/// Base64 value of one PEM character; 64 for anything else.
const fn b64(c: u8) -> u32 {
    match c {
        b'A'..=b'Z' => (c - b'A') as u32,
        b'a'..=b'z' => (c - b'a') as u32 + 26,
        b'0'..=b'9' => (c - b'0') as u32 + 52,
        b'+' => 62,
        b'/' => 63,
        _ => 64,
    }
}

/// Where `needle` starts in `hay` at or after `from`; `hay.len()` if nowhere.
const fn find(hay: &[u8], needle: &[u8], from: usize) -> usize {
    let mut i = from;
    while i + needle.len() <= hay.len() {
        let mut j = 0;
        while j < needle.len() && hay[i + j] == needle[j] {
            j += 1;
        }
        if j == needle.len() {
            return i;
        }
        i += 1;
    }
    hay.len()
}

/// The base64 body of the first certificate: between its BEGIN and END
/// lines - the same text the IoT tab reads.
const fn pem_body(pem: &[u8]) -> (usize, usize) {
    const BEGIN: &[u8] = b"-----BEGIN CERTIFICATE-----";
    const END: &[u8] = b"-----END CERTIFICATE-----";
    let begin = find(pem, BEGIN, 0);
    if begin == pem.len() {
        return (0, 0);
    }
    let start = begin + BEGIN.len();
    (start, find(pem, END, start))
}

/// Bytes of DER in `pem`.
const fn der_len(pem: &str) -> usize {
    let p = pem.as_bytes();
    let (mut i, end) = pem_body(p);
    let mut chars = 0;
    while i < end {
        if b64(p[i]) < 64 {
            chars += 1;
        }
        i += 1;
    }
    chars * 6 / 8
}

/// The DER inside `pem`.
const fn pem_to_der<const N: usize>(pem: &str) -> [u8; N] {
    let p = pem.as_bytes();
    let (mut i, end) = pem_body(p);
    let mut out = [0u8; N];
    let (mut acc, mut bits, mut o) = (0u32, 0u32, 0usize);
    while i < end {
        let v = b64(p[i]);
        if v < 64 {
            acc = (acc << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out[o] = (acc >> bits) as u8;
                o += 1;
            }
            acc &= (1 << bits) - 1;
        }
        i += 1;
    }
    assert!(N > 0 && out[0] == 0x30, "CA_PEM holds no PEM certificate");
    out
}
