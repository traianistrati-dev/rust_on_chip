
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
// With a client certificate (CLIENT, from secrets.rs) the board proves who it
// is when the broker asks - AWS IoT does: it sends the certificate and signs
// the handshake with its P-256 key (one scalar multiplication, no heap).
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
use embedded_tls::{
    Aes128GcmSha256, Certificate, CryptoProvider, CryptoRngCore, SignatureScheme, TlsConfig,
    TlsConnection, TlsContext, TlsError, TlsVerifier,
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

/// The CAs as DER, one after the other, decoded by the compiler: a broken
/// paste fails the build.
static CA_DER: [u8; pem_len(CA_PEM.as_bytes(), b"CERTIFICATE", true)] =
    pem_der(CA_PEM, b"CERTIFICATE", true, false, "CA_PEM in mqtt_ca.rs holds no PEM certificate");

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
    let config = if ALPN.is_empty() {
        config
    } else {
        config.with_alpn(ALPN)
    };
    if VERIFY && ip {
        return Err(());
    }
    let mut verifier = Verifier::new(&CA_DER[..]);
    let provider = Provider {
        rng: tls_rng()?,
        verifier: if VERIFY { Some(&mut verifier) } else { None },
    };
    link.open(TlsContext::new(&config, provider))
        .await
        .map_err(drop)?;
    // embedded-tls 0.19 also completes a handshake in which the server sent
    // no certificate at all (its issue #215): then nobody was authenticated.
    if VERIFY && !verifier.authenticated {
        return Err(());
    }
    // A broker that refuses the board's certificate says so on the first
    // read, not here: mqtt.rs then finds the connection closed and retries.
    Ok(link)
}

/// What embedded-tls asks for during the handshake: randomness, the verifier
/// that checks the broker's chain, its signature and its name (none: encrypt
/// only) - and, when the broker asks, the board's certificate and signature.
struct Provider<'v, 'a> {
    rng: TlsRng,
    verifier: Option<&'v mut Verifier<'a>>,
}

impl CryptoProvider for Provider<'_, '_> {
    type CipherSuite = Aes128GcmSha256;
    type Signature = p256::ecdsa::DerSignature;

    fn rng(&mut self) -> impl CryptoRngCore {
        &mut self.rng
    }

    fn verifier(&mut self) -> Result<&mut impl TlsVerifier<Aes128GcmSha256>, TlsError> {
        // None: embedded-tls checks nothing at all - the chain, the signature
        // and the name alike.
        self.verifier.as_deref_mut().ok_or(TlsError::Unimplemented)
    }

    fn signer(&mut self) -> Result<(impl ecdsa::signature::SignerMut<Self::Signature>, SignatureScheme), TlsError> {
        let (_, key) = CLIENT.ok_or(TlsError::Unimplemented)?;
        Ok((ClientKey::new(key)?, SignatureScheme::EcdsaSecp256r1Sha256))
    }

    fn client_cert(&mut self) -> Option<Certificate<impl AsRef<[u8]>>> {
        CLIENT.map(|(cert, _)| Certificate::X509(cert))
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

// ── The broker's certificate, and the board's ─────────────────────────────
// Everything from here down is shared, as it is, with the IoT tab's "Test
// from this PC", which runs the same checks on the PC against the same broker
// - full paths, so it stands in either file. It uses CertClock and MAX_CHAIN,
// and pem_der.rs (below).

/// embedded-tls's `CertVerifier` (each certificate's signature by the next,
/// the last by the CA, the name, the dates when CertClock knows the time),
/// plus what it leaves out:
///
/// - the CA may sit anywhere above the broker's certificate: the chain is cut
///   right after the first certificate one of `cas` issued (its issuer's name
///   is the CA's), so a cross-signed chain - AWS IoT's ends in Starfield's
///   roots, past Amazon Root CA 1 - still ends in mqtt_ca.rs. embedded-tls
///   walks from the LAST certificate sent;
/// - every certificate after the broker's own must be a CA - without it, a
///   chain [any name <- an ordinary certificate someone bought <- ... <- the
///   CA] is accepted;
/// - an RSA-PSS signature only from an RSA key: embedded-tls 0.19 unwraps the
///   key as RSA (pki.rs:214), and a replayed public EC chain would panic it.
struct Verifier<'a> {
    /// mqtt_ca.rs's CAs, one DER after the other.
    cas: &'a [u8],
    /// embedded-tls's checks, against the CA that issued the chain.
    inner: Option<embedded_tls::pki::CertVerifier<'a, embedded_tls::Aes128GcmSha256, CertClock, MAX_CHAIN>>,
    /// The name the broker's certificate must carry (SNI).
    host: [u8; 64],
    host_len: usize,
    /// The broker's chain passed.
    chain_ok: bool,
    /// ... its own certificate carries an RSA key ...
    leaf_rsa: bool,
    /// ... and it signed this handshake with that certificate's key.
    authenticated: bool,
}

impl<'a> Verifier<'a> {
    fn new(cas: &'a [u8]) -> Self {
        Self {
            cas,
            inner: None,
            host: [0; 64],
            host_len: 0,
            chain_ok: false,
            leaf_rsa: false,
            authenticated: false,
        }
    }

    /// The CA the chain ends in, and how many of its certificates to keep:
    /// up to the first one a CA of `cas` issued - its issuer's name is the
    /// CA's, and where both say which key, the same key (a CA renewed under
    /// its old name). None issued: the first CA and the whole chain, which
    /// then fails as it always did.
    fn anchor(&self, chain: &embedded_tls::CertificateRef) -> (&'a [u8], usize) {
        let mut best: Option<(&'a [u8], usize)> = None;
        let mut cas = self.cas;
        while let Some((0x30, _, rest)) = tlv(cas) {
            let ca = &cas[..cas.len() - rest.len()];
            cas = rest;
            let Some(subject) = cert_name(ca, 4) else {
                continue;
            };
            let ca_key = key_id(ca, &[0x55, 0x1D, 0x0E]);
            for (i, entry) in chain.entries.iter().enumerate() {
                let embedded_tls::CertificateEntryRef::X509(der) = entry else {
                    continue;
                };
                let other_key = matches!(
                    (ca_key, key_id(der, &[0x55, 0x1D, 0x23])),
                    (Some(ca_key), Some(issuer_key)) if ca_key != issuer_key
                );
                if cert_name(der, 2) == Some(subject) && !other_key && best.is_none_or(|(_, keep)| i + 1 < keep) {
                    best = Some((ca, i + 1));
                }
            }
        }
        let first = tlv(self.cas).map_or(self.cas, |(_, _, rest)| &self.cas[..self.cas.len() - rest.len()]);
        best.unwrap_or((first, chain.entries.len()))
    }
}

impl embedded_tls::TlsVerifier<embedded_tls::Aes128GcmSha256> for Verifier<'_> {
    fn set_hostname_verification(&mut self, hostname: &str) -> Result<(), embedded_tls::TlsError> {
        let name = hostname.as_bytes();
        if name.len() > self.host.len() {
            return Err(embedded_tls::TlsError::InsufficientSpace);
        }
        self.host[..name.len()].copy_from_slice(name);
        self.host_len = name.len();
        Ok(())
    }

    fn verify_certificate(
        &mut self,
        transcript: &embedded_tls::Sha256,
        chain: embedded_tls::CertificateRef,
    ) -> Result<(), embedded_tls::TlsError> {
        let mut chain = chain;
        let (ca, keep) = self.anchor(&chain);
        // In place: embedded-tls keeps its own copy of what was sent, for
        // checking the broker's signature against the first certificate.
        chain.entries.truncate(keep);
        for issuer in chain.entries.iter().skip(1) {
            match issuer {
                embedded_tls::CertificateEntryRef::X509(der) if is_ca(der) => {}
                _ => return Err(embedded_tls::TlsError::InvalidCertificate),
            }
        }
        let leaf_rsa = matches!(chain.entries.first(), Some(embedded_tls::CertificateEntryRef::X509(der)) if key_is_rsa(der));
        let mut inner = embedded_tls::pki::CertVerifier::new(embedded_tls::Certificate::X509(ca));
        if self.host_len > 0 {
            let host = core::str::from_utf8(&self.host[..self.host_len])
                .map_err(|_| embedded_tls::TlsError::InvalidCertificate)?;
            inner.set_hostname_verification(host)?;
        }
        inner.verify_certificate(transcript, chain)?;
        self.inner = Some(inner);
        self.leaf_rsa = leaf_rsa;
        self.chain_ok = true;
        Ok(())
    }

    fn verify_signature(
        &mut self,
        verify: embedded_tls::CertificateVerifyRef,
    ) -> Result<(), embedded_tls::TlsError> {
        // Without a checked chain first, embedded-tls 0.19 would panic here.
        let (true, Some(inner)) = (self.chain_ok, self.inner.as_mut()) else {
            return Err(embedded_tls::TlsError::InvalidCertificate);
        };
        let pss = matches!(
            verify.signature_scheme,
            embedded_tls::SignatureScheme::RsaPssRsaeSha256
                | embedded_tls::SignatureScheme::RsaPssRsaeSha384
                | embedded_tls::SignatureScheme::RsaPssRsaeSha512
        );
        if pss && !self.leaf_rsa {
            return Err(embedded_tls::TlsError::InvalidSignatureScheme);
        }
        inner.verify_signature(verify)?;
        self.authenticated = true;
        Ok(())
    }
}

/// The board's key, signing the handshake for its client certificate: one
/// scalar multiplication - RFC 6979, so no randomness - and no heap.
struct ClientKey(p256::NonZeroScalar);

impl ClientKey {
    fn new(d: [u8; 32]) -> Result<Self, embedded_tls::TlsError> {
        Option::from(p256::NonZeroScalar::from_repr(d.into()))
            .map(Self)
            .ok_or(embedded_tls::TlsError::InvalidPrivateKey)
    }
}

impl ecdsa::signature::Signer<p256::ecdsa::DerSignature> for ClientKey {
    fn try_sign(&self, message: &[u8]) -> Result<p256::ecdsa::DerSignature, ecdsa::Error> {
        use ecdsa::hazmat::SignPrimitive as _;
        use ecdsa::signature::digest::Digest as _;
        let digest = embedded_tls::Sha256::digest(message);
        let (signature, _) = self.0.try_sign_prehashed_rfc6979::<embedded_tls::Sha256>(&digest, &[])?;
        Ok(signature.to_der())
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

/// A field of a certificate's TBSCertificate, whole (tag and length too),
/// counted after its version: 2 = issuer, 4 = subject.
fn cert_name(cert: &[u8], field: usize) -> Option<&[u8]> {
    let (0x30, cert, _) = tlv(cert)? else {
        return None;
    };
    let (0x30, mut tbs, _) = tlv(cert)? else {
        return None;
    };
    if let Some((0xA0, _, rest)) = tlv(tbs) {
        tbs = rest; // the version
    }
    for _ in 0..field {
        tbs = tlv(tbs)?.2;
    }
    let (_, _, rest) = tlv(tbs)?;
    Some(&tbs[..tbs.len() - rest.len()])
}

/// A certificate extension's value (inside its OCTET STRING), by OID.
fn extension<'c>(cert: &'c [u8], oid: &[u8]) -> Option<&'c [u8]> {
    let (0x30, cert, _) = tlv(cert)? else {
        return None;
    };
    let (0x30, mut tbs, _) = tlv(cert)? else {
        return None;
    };
    while let Some((tag, value, rest)) = tlv(tbs) {
        tbs = rest;
        if tag != 0xA3 {
            continue;
        }
        let (0x30, mut extensions, _) = tlv(value)? else {
            return None;
        };
        while let Some((0x30, extension, rest)) = tlv(extensions) {
            extensions = rest;
            let (0x06, id, mut extension) = tlv(extension)? else {
                return None;
            };
            if id != oid {
                continue;
            }
            if let Some((0x01, _, rest)) = tlv(extension) {
                extension = rest; // the `critical` flag
            }
            let (0x04, value, _) = tlv(extension)? else {
                return None;
            };
            return Some(value);
        }
    }
    None
}

/// The key id a certificate names: its own (SubjectKeyIdentifier,
/// 2.5.29.14) or its issuer's (AuthorityKeyIdentifier's keyIdentifier,
/// 2.5.29.35).
fn key_id<'c>(cert: &'c [u8], oid: &[u8]) -> Option<&'c [u8]> {
    let value = extension(cert, oid)?;
    if oid == [0x55, 0x1D, 0x0E] {
        let (0x04, id, _) = tlv(value)? else {
            return None;
        };
        return Some(id);
    }
    let (0x30, aki, _) = tlv(value)? else {
        return None;
    };
    let (0x80, id, _) = tlv(aki)? else {
        return None;
    };
    Some(id)
}

/// `true` when the certificate's basicConstraints say cA = TRUE.
fn is_ca(cert: &[u8]) -> bool {
    let Some((0x30, cert, _)) = tlv(cert) else {
        return false;
    };
    let Some((0x30, mut tbs, _)) = tlv(cert) else {
        return false;
    };
    // The extensions are TBSCertificate's [3]; skip everything before them.
    while let Some((tag, value, rest)) = tlv(tbs) {
        tbs = rest;
        if tag != 0xA3 {
            continue;
        }
        let Some((0x30, mut extensions, _)) = tlv(value) else {
            return false;
        };
        while let Some((0x30, extension, rest)) = tlv(extensions) {
            extensions = rest;
            let Some((0x06, oid, mut extension)) = tlv(extension) else {
                return false;
            };
            if oid != [0x55, 0x1D, 0x13] {
                continue; // not basicConstraints (2.5.29.19)
            }
            if let Some((0x01, _, rest)) = tlv(extension) {
                extension = rest; // the `critical` flag
            }
            let Some((0x04, value, _)) = tlv(extension) else {
                return false;
            };
            let Some((0x30, constraints, _)) = tlv(value) else {
                return false;
            };
            return matches!(tlv(constraints), Some((0x01, [0xFF], _)));
        }
        return false;
    }
    false
}

/// `true` when the certificate's key is RSA (rsaEncryption, 1.2.840.113549.1.1.1).
fn key_is_rsa(cert: &[u8]) -> bool {
    let Some((0x30, cert, _)) = tlv(cert) else {
        return false;
    };
    let Some((0x30, mut tbs, _)) = tlv(cert) else {
        return false;
    };
    if let Some((0xA0, _, rest)) = tlv(tbs) {
        tbs = rest; // the version
    }
    // serial, signature, issuer, validity, subject - then the key
    for _ in 0..5 {
        let Some((_, _, rest)) = tlv(tbs) else {
            return false;
        };
        tbs = rest;
    }
    let Some((0x30, key, _)) = tlv(tbs) else {
        return false;
    };
    let Some((0x30, algorithm, _)) = tlv(key) else {
        return false;
    };
    matches!(
        tlv(algorithm),
        Some((
            0x06,
            [0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01],
            _
        ))
    )
}

// ── PEM and DER, decoded by the compiler ──────────────────────────────────
// Shared, as it is, by the board (mqtt_link.rs), the IoT tab and "Test from
// this PC": what the tab says of a certificate or a key is what the board
// builds. `const fn`s only - so no slicing and no `for` loops.

/// Base64 value of one PEM character; 64 for anything else.
pub const fn b64(c: u8) -> u32 {
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
pub const fn find(hay: &[u8], needle: &[u8], from: usize) -> usize {
    let mut i = from;
    while i + needle.len() <= hay.len() {
        if holds(hay, i, needle) {
            return i;
        }
        i += 1;
    }
    hay.len()
}

/// `hay` holds `needle` at `at`.
pub const fn holds(hay: &[u8], at: usize, needle: &[u8]) -> bool {
    if at + needle.len() > hay.len() {
        return false;
    }
    let mut j = 0;
    while j < needle.len() {
        if hay[at + j] != needle[j] {
            return false;
        }
        j += 1;
    }
    true
}

/// Where `needle` starts in `hay` between `from` and `to`; `to` if nowhere.
pub const fn find_to(hay: &[u8], needle: &[u8], from: usize, to: usize) -> usize {
    let mut i = from;
    while i + needle.len() <= to && i + needle.len() <= hay.len() {
        if holds(hay, i, needle) {
            return i;
        }
        i += 1;
    }
    to
}

/// The next `-----BEGIN <label>-----` block of `pem` from `from`: (where its
/// base64 starts, where it ends, where to look for the next) - `None` when
/// there is none. A block whose headers say it is encrypted
/// (`Proc-Type: 4,ENCRYPTED`) has an empty body. One pass over the text,
/// however many blocks: the compiler's constant evaluation has a step limit.
pub const fn pem_next(pem: &[u8], label: &[u8], from: usize) -> Option<(usize, usize, usize)> {
    const BEGIN: &[u8] = b"-----BEGIN ";
    const DASHES: &[u8] = b"-----";
    const END: &[u8] = b"-----END ";
    let mut i = from;
    loop {
        i = find(pem, BEGIN, i);
        if i == pem.len() {
            return None;
        }
        let at = i + BEGIN.len();
        if holds(pem, at, label) && holds(pem, at + label.len(), DASHES) {
            let start = at + label.len() + DASHES.len();
            let end = find(pem, END, start);
            let next = if end < pem.len() { end + END.len() } else { end };
            if find_to(pem, b"Proc-Type:", start, end) < end {
                return Some((start, start, next));
            }
            return Some((start, end, next));
        }
        i = at;
    }
}

/// Bytes of DER in the first `label` block of `pem` - or in all of them.
pub const fn pem_len(pem: &[u8], label: &[u8], all: bool) -> usize {
    let mut total = 0;
    let mut from = 0;
    while let Some((mut i, end, next)) = pem_next(pem, label, from) {
        let mut chars = 0;
        while i < end {
            if b64(pem[i]) < 64 {
                chars += 1;
            }
            i += 1;
        }
        total += chars * 6 / 8;
        if !all {
            break;
        }
        from = next;
    }
    total
}

/// The DER of the first `label` block of `pem` - or of all of them, one after
/// the other - into `out`; the bytes written.
pub const fn pem_decode(pem: &[u8], label: &[u8], all: bool, out: &mut [u8]) -> usize {
    let mut o = 0;
    let mut from = 0;
    while let Some((mut i, end, next)) = pem_next(pem, label, from) {
        let (mut acc, mut bits) = (0u32, 0u32);
        while i < end {
            let v = b64(pem[i]);
            if v < 64 {
                acc = (acc << 6) | v;
                bits += 6;
                if bits >= 8 {
                    bits -= 8;
                    if o < out.len() {
                        out[o] = (acc >> bits) as u8;
                    }
                    o += 1;
                }
                acc &= (1 << bits) - 1;
            }
            i += 1;
        }
        if !all {
            break;
        }
        from = next;
    }
    o
}

/// Nothing but spaces and line ends.
pub const fn blank(text: &[u8]) -> bool {
    let mut i = 0;
    while i < text.len() {
        if !matches!(text[i], b' ' | b'\t' | b'\r' | b'\n') {
            return false;
        }
        i += 1;
    }
    true
}

/// `pem`'s `label` block(s) as DER for a `static`: `N` from [`pem_len`]. A
/// paste that holds none fails the BUILD with `what` - and, `optional`, an
/// empty text gives no bytes instead.
pub const fn pem_der<const N: usize>(pem: &str, label: &[u8], all: bool, optional: bool, what: &str) -> [u8; N] {
    let mut out = [0u8; N];
    let n = pem_decode(pem.as_bytes(), label, all, &mut out);
    let none = N == 0 && optional && blank(pem.as_bytes());
    assert!(none || (N > 0 && n == N && out[0] == 0x30), "{}", what);
    out
}

/// The labels a P-256 key comes under: SEC1 (`openssl ecparam -genkey`, the
/// IoT tab) and PKCS#8 (`openssl genpkey`).
pub const KEY_LABELS: [&[u8]; 2] = [b"EC PRIVATE KEY", b"PRIVATE KEY"];

/// The label `key` is under, of [`KEY_LABELS`]; the first when neither.
pub const fn key_label(key: &[u8]) -> &'static [u8] {
    if pem_len(key, KEY_LABELS[0], false) > 0 {
        KEY_LABELS[0]
    } else {
        KEY_LABELS[1]
    }
}

/// One DER element at `at` in `d`: (tag, where its contents start, their
/// length) - `None` when it runs past `d`.
pub const fn der_at(d: &[u8], at: usize) -> Option<(u8, usize, usize)> {
    if at + 2 > d.len() {
        return None;
    }
    let tag = d[at];
    let first = d[at + 1];
    let mut start = at + 2;
    let mut len = first as usize;
    if first >= 0x80 {
        let n = (first & 0x7F) as usize;
        if n == 0 || n > 3 || start + n > d.len() {
            return None;
        }
        len = 0;
        let mut k = 0;
        while k < n {
            len = (len << 8) | d[start + k] as usize;
            k += 1;
        }
        start += n;
    }
    if start + len > d.len() {
        return None;
    }
    Some((tag, start, len))
}

/// `d` holds exactly `want` as the `len` bytes at `at`.
const fn same(d: &[u8], at: usize, len: usize, want: &[u8]) -> bool {
    len == want.len() && holds(d, at, want)
}

/// id-ecPublicKey (1.2.840.10045.2.1) and prime256v1 (1.2.840.10045.3.1.7).
const EC_PUBLIC_KEY: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];
const PRIME256V1: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07];

/// A P-256 key: its scalar, and its public point (`04 X Y`) when the key
/// carries one - from SEC1 or PKCS#8 DER. `None` for anything else: RSA,
/// another curve, a zero key, a broken paste.
pub const fn ec_key(der: &[u8]) -> Option<([u8; 32], Option<[u8; 65]>)> {
    let Some((0x30, body, body_len)) = der_at(der, 0) else {
        return None;
    };
    let Some((0x02, v, 1)) = der_at(der, body) else {
        return None;
    };
    if der[v] == 1 {
        // SEC1 ECPrivateKey: its curve in [0].
        return sec1(der, v + 1, body + body_len, false);
    }
    if der[v] != 0 {
        return None;
    }
    // PKCS#8: the curve in the AlgorithmIdentifier, then SEC1 in an OCTET STRING.
    let Some((0x30, alg, alg_len)) = der_at(der, v + 1) else {
        return None;
    };
    let Some((0x06, oid, oid_len)) = der_at(der, alg) else {
        return None;
    };
    if !same(der, oid, oid_len, EC_PUBLIC_KEY) {
        return None;
    }
    let Some((0x06, curve, curve_len)) = der_at(der, oid + oid_len) else {
        return None;
    };
    if !same(der, curve, curve_len, PRIME256V1) {
        return None;
    }
    let Some((0x04, inner, _)) = der_at(der, alg + alg_len) else {
        return None;
    };
    let Some((0x30, seq, seq_len)) = der_at(der, inner) else {
        return None;
    };
    let Some((0x02, v, 1)) = der_at(der, seq) else {
        return None;
    };
    if der[v] != 1 {
        return None;
    }
    sec1(der, v + 1, seq + seq_len, true)
}

/// The rest of a SEC1 ECPrivateKey from `at` (after its version) to `end`.
const fn sec1(der: &[u8], at: usize, end: usize, mut curve_known: bool) -> Option<([u8; 32], Option<[u8; 65]>)> {
    let Some((0x04, key, key_len)) = der_at(der, at) else {
        return None;
    };
    if key_len == 0 || key_len > 32 {
        return None;
    }
    let mut d = [0u8; 32];
    let mut nonzero = false;
    let mut k = 0;
    while k < key_len {
        d[32 - key_len + k] = der[key + k];
        nonzero |= der[key + k] != 0;
        k += 1;
    }
    if !nonzero {
        return None;
    }
    let mut point = None;
    let mut i = key + key_len;
    while i < end {
        let Some((tag, inner, inner_len)) = der_at(der, i) else {
            return None;
        };
        if tag == 0xA0 {
            let Some((0x06, curve, curve_len)) = der_at(der, inner) else {
                return None;
            };
            if !same(der, curve, curve_len, PRIME256V1) {
                return None;
            }
            curve_known = true;
        } else if tag == 0xA1 {
            // The public half: uncompressed (`04 X Y`), kept to match the
            // certificate against; compressed (`02/03 X`) is valid too, and
            // simply not compared here.
            let Some((0x03, bits, len)) = der_at(der, inner) else {
                return None;
            };
            if len == 66 && der[bits] == 0 && der[bits + 1] == 0x04 {
                let mut p = [0u8; 65];
                let mut k = 0;
                while k < 65 {
                    p[k] = der[bits + 1 + k];
                    k += 1;
                }
                point = Some(p);
            } else if !(len == 34 && der[bits] == 0 && (der[bits + 1] == 0x02 || der[bits + 1] == 0x03)) {
                return None;
            }
        }
        i = inner + inner_len;
    }
    if !curve_known {
        return None;
    }
    Some((d, point))
}

/// A certificate's SubjectPublicKeyInfo: (where its BIT STRING's bytes
/// start, their length) when the key is P-256 - `None` for any other key.
const fn cert_p256_bits(der: &[u8]) -> Option<(usize, usize)> {
    let Some((0x30, cert, _)) = der_at(der, 0) else {
        return None;
    };
    let Some((0x30, tbs, _)) = der_at(der, cert) else {
        return None;
    };
    let mut i = tbs;
    let mut field = 0;
    // [0] version, then serial, signature, issuer, validity, subject.
    while field < 6 {
        let Some((tag, inner, len)) = der_at(der, i) else {
            return None;
        };
        if field == 0 && tag != 0xA0 {
            field += 1;
            continue;
        }
        i = inner + len;
        field += 1;
    }
    let Some((0x30, spki, _)) = der_at(der, i) else {
        return None;
    };
    let Some((0x30, alg, alg_len)) = der_at(der, spki) else {
        return None;
    };
    let Some((0x06, oid, oid_len)) = der_at(der, alg) else {
        return None;
    };
    if !same(der, oid, oid_len, EC_PUBLIC_KEY) {
        return None;
    }
    let Some((0x06, curve, curve_len)) = der_at(der, oid + oid_len) else {
        return None;
    };
    if !same(der, curve, curve_len, PRIME256V1) {
        return None;
    }
    let Some((0x03, bits, len)) = der_at(der, alg + alg_len) else {
        return None;
    };
    Some((bits, len))
}

/// The certificate's key is P-256 (stored either way).
pub const fn cert_is_p256(der: &[u8]) -> bool {
    cert_p256_bits(der).is_some()
}

/// A certificate's public key when it is P-256, stored uncompressed: its
/// point (`04 X Y`).
pub const fn cert_ec_point(der: &[u8]) -> Option<[u8; 65]> {
    let Some((bits, 66)) = cert_p256_bits(der) else {
        return None;
    };
    if der[bits] != 0 || der[bits + 1] != 0x04 {
        return None;
    }
    let mut p = [0u8; 65];
    let mut k = 0;
    while k < 65 {
        p[k] = der[bits + 1 + k];
        k += 1;
    }
    Some(p)
}

/// The board's identity for a client certificate: the certificate's DER and
/// the key's scalar - `None` while either is empty in secrets.rs. A key that
/// is not P-256, a certificate whose key is not P-256, or a pair that does
/// not belong together fails the BUILD.
pub const fn client_identity(cert: &'static [u8], key: &[u8]) -> Option<(&'static [u8], [u8; 32])> {
    if cert.is_empty() || key.is_empty() {
        return None;
    }
    let Some((d, point)) = ec_key(key) else {
        panic!("MQTT_CLIENT_KEY in secrets.rs is not a P-256 key (SEC1 or PKCS#8): the board signs with P-256 only");
    };
    if !cert_is_p256(cert) {
        panic!("MQTT_CLIENT_CERT in secrets.rs does not carry a P-256 key: the board signs with P-256 only");
    }
    // Compared when both halves are written out uncompressed - as every key
    // and certificate the IoT tab makes is.
    if let (Some(p), Some(c)) = (point, cert_ec_point(cert)) {
        let mut k = 0;
        while k < 65 {
            if p[k] != c[k] {
                panic!("MQTT_CLIENT_CERT in secrets.rs is not the certificate of MQTT_CLIENT_KEY");
            }
            k += 1;
        }
    }
    Some((cert, d))
}
