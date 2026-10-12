//! RustOnChip's "Test from this PC" (IoT tab, MQTT over TLS).
//!
//! The board's own certificate checks - `tls_verify.rs`, the very text the
//! firmware's `mqtt_link.rs` carries - run on this PC against the broker, then
//! an MQTT 5 CONNECT as the firmware sends it. The IDE writes this program,
//! builds it once, and runs it with the request on stdin: one hex-encoded
//! value per line - host, port, server name, verify ("1"/"0"), the CAs as DER,
//! client id, user name, password, AWS IoT's ALPN ("1"/"0"), the client
//! certificate and its key as PEM (empty: none). It prints `INFO ...` lines,
//! then one `RESULT ok ...` or `RESULT fail ...`.
//!
//! `csr` as its argument: a P-256 key and a certificate request for it.
//! stdin: country, organization, common name, and a key as PEM (empty: make
//! one). It prints `KEY <hex PEM>` (a key it made), `CSR <hex PEM>`, then the
//! `RESULT` line.
#![allow(dead_code)]

use std::cell::Cell;
use std::io::BufRead;
use std::net::{Ipv4Addr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use embedded_io_adapters::std::FromStd;
use embedded_tls::blocking::TlsConnection;
use embedded_tls::{Aes128GcmSha256, Certificate, SignatureScheme, TlsConfig, TlsContext};

/// The certificates' dates are checked against this PC's clock. (The board
/// checks them only once SNTP has the time.)
type CertClock = std::time::SystemTime;
/// The longest certificate chain the broker may send - as on the board.
const MAX_CHAIN: usize = 8192;

include!("tls_verify.rs");
include!("pem_der.rs");
include!("csr_der.rs");

struct Request {
    host: String,
    port: u16,
    server_name: String,
    verify: bool,
    ca: Vec<u8>,
    client_id: String,
    user: String,
    password: String,
    alpn: bool,
    client_cert: String,
    client_key: String,
}

fn unhex(line: &str) -> Vec<u8> {
    let line = line.trim();
    (0..line.len() / 2)
        .filter_map(|i| u8::from_str_radix(&line[2 * i..2 * i + 2], 16).ok())
        .collect()
}

fn stdin_values() -> Vec<Vec<u8>> {
    let stdin = std::io::stdin();
    stdin.lock().lines().map_while(Result::ok).map(|l| unhex(&l)).collect()
}

fn read_request() -> Option<Request> {
    let lines = stdin_values();
    let text = |i: usize| lines.get(i).map(|b| String::from_utf8_lossy(b).into_owned());
    Some(Request {
        host: text(0)?,
        port: text(1)?.parse().ok()?,
        server_name: text(2)?,
        verify: text(3)? == "1",
        ca: lines.get(4)?.clone(),
        client_id: text(5)?,
        user: text(6)?,
        password: text(7)?,
        alpn: text(8)? == "1",
        client_cert: text(9)?,
        client_key: text(10)?,
    })
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("csr") {
        match csr() {
            Ok(what) => println!("RESULT ok {what}"),
            Err(why) => println!("RESULT fail {why}"),
        }
        return;
    }
    let Some(request) = read_request() else {
        println!("RESULT fail the IDE sent no readable request");
        return;
    };
    match run(&request) {
        Ok(what) => println!("RESULT ok {what}"),
        Err(why) => println!("RESULT fail {why}"),
    }
}

/// The firmware's verifier, and what this PC also wants to know: whether the
/// chain needs P-384 (the board checks it only with "P-384 chains").
struct HostVerifier<'a> {
    inner: Verifier<'a>,
    p384: bool,
    /// The broker's certificate arrived (a failure before it is in the hello).
    chain_seen: bool,
}

impl embedded_tls::TlsVerifier<Aes128GcmSha256> for HostVerifier<'_> {
    fn set_hostname_verification(&mut self, hostname: &str) -> Result<(), embedded_tls::TlsError> {
        self.inner.set_hostname_verification(hostname)
    }

    fn verify_certificate(
        &mut self,
        transcript: &embedded_tls::Sha256,
        chain: embedded_tls::CertificateRef,
    ) -> Result<(), embedded_tls::TlsError> {
        // Only what the board checks: the chain up to the CA it ends in.
        let (_, keep) = self.inner.anchor(&chain);
        self.p384 |= chain
            .entries
            .iter()
            .take(keep)
            .any(|e| matches!(e, embedded_tls::CertificateEntryRef::X509(der) if signed_with_p384(der)));
        self.chain_seen = true;
        self.inner.verify_certificate(transcript, chain)
    }

    fn verify_signature(&mut self, verify: embedded_tls::CertificateVerifyRef) -> Result<(), embedded_tls::TlsError> {
        self.p384 |= matches!(verify.signature_scheme, embedded_tls::SignatureScheme::EcdsaSecp384r1Sha384);
        self.inner.verify_signature(verify)
    }
}

/// `true` when the certificate was signed with ecdsa-with-SHA384 (a P-384 key).
fn signed_with_p384(cert: &[u8]) -> bool {
    let Some((0x30, cert, _)) = tlv(cert) else { return false };
    let Some((_, _, after_tbs)) = tlv(cert) else { return false };
    let Some((0x30, algorithm, _)) = tlv(after_tbs) else { return false };
    matches!(
        tlv(algorithm),
        Some((0x06, [0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x03], _))
    )
}

/// As the board's, plus what this PC reports: did the broker ask for the
/// client certificate, and was the handshake signed with its key.
struct Provider<'v, 'a> {
    rng: rand_core::OsRng,
    verifier: Option<&'v mut HostVerifier<'a>>,
    client: Option<(&'v [u8], [u8; 32])>,
    asked: &'v Cell<bool>,
    signed: &'v Cell<bool>,
}

impl embedded_tls::CryptoProvider for Provider<'_, '_> {
    type CipherSuite = Aes128GcmSha256;
    type Signature = p256::ecdsa::DerSignature;

    fn rng(&mut self) -> impl embedded_tls::CryptoRngCore {
        &mut self.rng
    }

    fn verifier(&mut self) -> Result<&mut impl embedded_tls::TlsVerifier<Aes128GcmSha256>, embedded_tls::TlsError> {
        self.verifier.as_deref_mut().ok_or(embedded_tls::TlsError::Unimplemented)
    }

    fn signer(&mut self) -> Result<(impl ecdsa::signature::SignerMut<Self::Signature>, SignatureScheme), embedded_tls::TlsError> {
        let (_, key) = self.client.ok_or(embedded_tls::TlsError::Unimplemented)?;
        self.signed.set(true);
        Ok((ClientKey::new(key)?, SignatureScheme::EcdsaSecp256r1Sha256))
    }

    fn client_cert(&mut self) -> Option<Certificate<impl AsRef<[u8]>>> {
        // Called only when the broker asked.
        self.asked.set(true);
        self.client.map(|(cert, _)| Certificate::X509(cert))
    }
}

/// The board's identity from the PEM text the tab holds, read by the
/// board's own code (`pem_der.rs`) - and, as the build would, refused when it
/// does not hold together. The key's public point, when the key lacks one, is
/// worked out here and compared too.
fn client_identity_of(cert_pem: &str, key_pem: &str) -> Result<Option<(Vec<u8>, [u8; 32])>, String> {
    let (cert_text, key_text) = (cert_pem.as_bytes(), key_pem.as_bytes());
    if blank(cert_text) || blank(key_text) {
        return Ok(None);
    }
    let mut cert = vec![0u8; pem_len(cert_text, b"CERTIFICATE", false)];
    pem_decode(cert_text, b"CERTIFICATE", false, &mut cert);
    let label = key_label(key_text);
    let mut key = vec![0u8; pem_len(key_text, label, false)];
    pem_decode(key_text, label, false, &mut key);
    let Some((d, point)) = ec_key(&key) else {
        return Err("MQTT_CLIENT_KEY in secrets.rs is not a P-256 key - the board would not build".into());
    };
    if !cert_is_p256(&cert) {
        return Err("MQTT_CLIENT_CERT in secrets.rs does not carry a P-256 key - the board would not build".into());
    }
    let Some(cert_point) = cert_ec_point(&cert) else {
        // Stored compressed: not matched here.
        return Ok(Some((cert, d)));
    };
    let point = match point {
        Some(p) => p,
        None => {
            let secret = p256::SecretKey::from_bytes(&d.into()).map_err(|_| "MQTT_CLIENT_KEY is not a usable P-256 key".to_string())?;
            use p256::elliptic_curve::sec1::ToEncodedPoint as _;
            let encoded = secret.public_key().to_encoded_point(false);
            encoded.as_bytes().try_into().map_err(|_| "a P-256 point of another length".to_string())?
        }
    };
    if point != cert_point {
        return Err("MQTT_CLIENT_CERT in secrets.rs is not the certificate of MQTT_CLIENT_KEY: the broker would refuse the board".into());
    }
    Ok(Some((cert, d)))
}

/// What a refused handshake means, in the user's terms.
fn explain(e: &embedded_tls::TlsError) -> &'static str {
    use embedded_tls::TlsError as E;
    match e {
        E::InvalidCertificate => {
            "the broker's certificate did not check out: its chain does not end in mqtt_ca.rs's CA, it does not name the server name, or it is out of date"
        }
        E::InvalidSignatureScheme | E::InvalidSignature => {
            "a signature the board cannot check: a key or curve embedded-tls does not take"
        }
        E::HandshakeAborted(..) => "the broker refused the handshake: it may speak only TLS 1.2",
        E::Io(_) | E::IoError | E::ConnectionClosed => {
            "the connection broke during the handshake - a broker that speaks only TLS 1.2 (an AWS IoT endpoint without a TLS 1.3 security policy) hangs up like this"
        }
        _ => "the handshake failed",
    }
}

/// Why a broker hangs up right after a handshake in which it asked for the
/// board's certificate.
const REFUSED_CERT: &str = "the broker closed the connection after the handshake: it refused the client certificate. On AWS IoT: the certificate is not registered or not ACTIVE, no policy attached to it allows iot:Connect for this client id, the thing is not attached to it, or the client id is not the thing's name";

fn run(r: &Request) -> Result<String, String> {
    let address = (r.host.as_str(), r.port)
        .to_socket_addrs()
        .map_err(|e| format!("{} could not be resolved: {e}", r.host))?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| format!("{} has no IPv4 address", r.host))?;
    println!("INFO resolved {address}");
    let tcp = TcpStream::connect_timeout(&address, Duration::from_secs(10))
        .map_err(|e| format!("no TCP connection to {address}: {e}"))?;
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = tcp.set_write_timeout(Some(Duration::from_secs(15)));
    println!("INFO TCP connected");

    let mut rx = vec![0u8; 16_640];
    let mut tx = vec![0u8; 2048];
    let mut tls: TlsConnection<'_, FromStd<TcpStream>, Aes128GcmSha256> =
        TlsConnection::new(FromStd::new(tcp), &mut rx, &mut tx);
    let ip = r.server_name.parse::<Ipv4Addr>().is_ok();
    let config = if ip {
        TlsConfig::new()
    } else {
        TlsConfig::new().with_server_name(&r.server_name)
    };
    const AWS_ALPN: &[&[u8]] = &[b"x-amzn-mqtt-ca"];
    let config = if r.alpn { config.with_alpn(AWS_ALPN) } else { config };
    if r.verify && ip {
        return Err("the server name is an address, and a certificate names DNS names".into());
    }
    let identity = client_identity_of(&r.client_cert, &r.client_key)?;
    if identity.is_some() {
        println!("INFO client certificate and key belong together (P-256)");
    }
    let (asked, signed) = (Cell::new(false), Cell::new(false));
    let mut verifier = HostVerifier { inner: Verifier::new(&r.ca), p384: false, chain_seen: false };
    let provider = Provider {
        rng: rand_core::OsRng,
        verifier: if r.verify { Some(&mut verifier) } else { None },
        client: identity.as_ref().map(|(cert, d)| (cert.as_slice(), *d)),
        asked: &asked,
        signed: &signed,
    };
    let opened = tls.open(TlsContext::new(&config, provider));
    if let Err(e) = opened {
        // embedded-tls reads the broker's certificate request too, and some
        // list a signature it does not know: then nothing of the chain came.
        if r.verify && !verifier.chain_seen && matches!(e, embedded_tls::TlsError::DecodeError) {
            return Err(format!("embedded-tls could not read the broker's handshake - likely its request for a client certificate ({e:?})"));
        }
        return Err(format!("{} ({e:?})", explain(&e)));
    }
    if r.verify {
        if !verifier.inner.authenticated {
            return Err("the broker finished the handshake without proving a certificate".into());
        }
        println!(
            "INFO certificate checked: its chain ends in a CA of mqtt_ca.rs, it names {}, and it is in date by this PC's clock",
            r.server_name
        );
        if verifier.p384 {
            println!("INFO p384");
        }
    } else {
        println!("INFO encrypted, the certificate not checked (encrypt only)");
    }
    match (asked.get(), identity.is_some()) {
        (true, true) if signed.get() => println!("INFO the broker asked for the client certificate: sent, and the handshake signed with its key"),
        (true, false) => println!("INFO the broker asked for a client certificate, and none is set"),
        (false, true) => println!("INFO the broker did not ask for the client certificate, so it was not sent"),
        _ => {}
    }
    // What a hang-up from here on means - a broker that asked for the
    // board's certificate and then closes is refusing it. (Silence is not a
    // hang-up.)
    let has_identity = identity.is_some();
    let refused = |e: String| {
        // A read that times out is `TimedOut` on Windows, `Other` (from
        // `WouldBlock`) on Linux and macOS.
        if !asked.get() || e.contains("TimedOut") || e.contains("Io(Other)") {
            e
        } else if !has_identity {
            format!("the broker wants a client certificate and hangs up without one: switch \"Client certificate\" on and set it in secrets.rs ({e})")
        } else {
            format!("{REFUSED_CERT} ({e})")
        }
    };

    // MQTT 5 CONNECT, as the board sends it. The credentials only to a
    // broker whose certificate was checked.
    let credentials = r.verify;
    let packet = connect_packet(&r.client_id, credentials.then_some((r.user.as_str(), r.password.as_str())));
    // `write` takes what fits in one record (about 1.9 KB): a long token as
    // the password needs more than one.
    let mut rest = packet.as_slice();
    while !rest.is_empty() {
        match tls.write(rest) {
            Ok(0) => return Err("the CONNECT could not be sent (no room)".into()),
            Ok(n) => rest = &rest[n..],
            Err(e) => return Err(refused(format!("the CONNECT could not be sent: {e:?}"))),
        }
    }
    tls.flush().map_err(|e| refused(format!("the CONNECT could not be sent: {e:?}")))?;
    let reason = read_connack(&mut tls).map_err(refused)?;
    let _ = tls.write(&[0xE0, 0x00]);
    let _ = tls.flush();
    let untested = if identity.is_some() && !asked.get() {
        " - the client certificate untested: this broker never asks for one (test.mosquitto.org asks on 8884 only)"
    } else {
        ""
    };
    match reason {
        0x00 => Ok(if credentials {
            format!("the broker accepted the connection (CONNACK: success){untested}")
        } else {
            format!("the broker accepted the connection without credentials (encrypt only sends none from here){untested}")
        }),
        // A login refused that was never sent: the board sends secrets.rs's
        // whatever the check, so TLS is all this could test.
        code @ (0x04 | 0x05 | 0x86 | 0x87) if !credentials => Ok(format!(
            "TLS works; the broker wants a login (0x{code:02X}), which encrypt only does not send from this PC - the board sends secrets.rs's, so turn the check on to test it"
        )),
        code => Err(format!("the broker refused the CONNECT: {} (0x{code:02X})", connack_reason(code))),
    }
}

/// An MQTT 5 CONNECT: clean start, keep-alive 60 s, no properties.
fn connect_packet(client_id: &str, credentials: Option<(&str, &str)>) -> Vec<u8> {
    let field = |out: &mut Vec<u8>, s: &[u8]| {
        out.extend_from_slice(&(s.len() as u16).to_be_bytes());
        out.extend_from_slice(s);
    };
    let mut flags = 0x02u8;
    let mut body = vec![0x00, 0x04, b'M', b'Q', b'T', b'T', 0x05];
    let mut payload = Vec::new();
    field(&mut payload, client_id.as_bytes());
    if let Some((user, password)) = credentials {
        if !user.is_empty() {
            flags |= 0x80;
            field(&mut payload, user.as_bytes());
        }
        if !password.is_empty() {
            flags |= 0x40;
            field(&mut payload, password.as_bytes());
        }
    }
    body.push(flags);
    body.extend_from_slice(&60u16.to_be_bytes());
    body.push(0x00); // no properties
    body.extend_from_slice(&payload);
    let mut packet = vec![0x10];
    let mut len = body.len();
    loop {
        let mut byte = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            byte |= 0x80;
        }
        packet.push(byte);
        if len == 0 {
            break;
        }
    }
    packet.extend_from_slice(&body);
    packet
}

/// The CONNACK's reason code (its second byte).
fn read_connack(tls: &mut TlsConnection<'_, FromStd<TcpStream>, Aes128GcmSha256>) -> Result<u8, String> {
    let mut byte = |what: &str| -> Result<u8, String> {
        let mut b = [0u8; 1];
        match tls.read(&mut b) {
            Ok(1) => Ok(b[0]),
            Ok(_) => Err(format!("the broker closed the connection before {what}")),
            Err(e) => Err(format!("{what} did not come from the broker ({e:?})")),
        }
    };
    let kind = byte("a CONNACK")?;
    if kind != 0x20 {
        return Err(format!("the broker answered something else than a CONNACK (0x{kind:02X})"));
    }
    let mut len = 0usize;
    for shift in [0, 7, 14, 21] {
        let b = byte("the CONNACK")?;
        len |= usize::from(b & 0x7F) << shift;
        if b & 0x80 == 0 {
            break;
        }
    }
    if len < 2 {
        return Err("a CONNACK too short to read".into());
    }
    let _flags = byte("the CONNACK")?;
    byte("the CONNACK")
}

fn connack_reason(code: u8) -> &'static str {
    match code {
        0x01 => "it speaks MQTT 3.1.1 only, and the board speaks MQTT 5",
        0x80 => "an unspecified error",
        0x84 => "it does not take MQTT 5",
        0x85 => "the client id is not valid",
        0x86 | 0x04 => "bad user name or password (secrets.rs)",
        0x87 | 0x05 => "not authorized",
        0x88 | 0x03 => "the server is unavailable",
        0x89 => "the server is busy",
        0x8A => "banned",
        0x8C => "a bad authentication method",
        0x95 => "packet too large",
        0x9F => "too many connections lately",
        _ => "another reason",
    }
}

/// The `csr` mode: a P-256 key (made here, or given) and its certificate
/// request, both as PEM on stdout - checked before they are printed: the
/// request's signature verifies with the key it names.
fn csr() -> Result<String, String> {
    use p256::ecdsa::signature::{Signer as _, Verifier as _};
    use p256::elliptic_curve::sec1::ToEncodedPoint as _;
    let lines = stdin_values();
    let text = |i: usize| lines.get(i).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
    let (country, organization, common_name, key_pem) = (text(0), text(1), text(2), text(3));
    let subject = Subject {
        country: &country,
        organization: &organization,
        common_name: &common_name,
    };
    if let Some(why) = subject.problem() {
        return Err(why.into());
    }
    let made = blank(key_pem.as_bytes());
    let secret = if made {
        p256::SecretKey::random(&mut rand_core::OsRng)
    } else {
        let p = key_pem.as_bytes();
        let label = key_label(p);
        let mut der = vec![0u8; pem_len(p, label, false)];
        pem_decode(p, label, false, &mut der);
        let (d, _) = ec_key(&der).ok_or("the key in secrets.rs is not a P-256 key")?;
        p256::SecretKey::from_bytes(&d.into()).map_err(|_| "the key in secrets.rs is not a usable P-256 key")?
    };
    let encoded = secret.public_key().to_encoded_point(false);
    let point: [u8; 65] = encoded.as_bytes().try_into().map_err(|_| "a P-256 point of another length")?;
    let info = request_info(&subject, &point);
    let signing = p256::ecdsa::SigningKey::from(&secret);
    let signature: p256::ecdsa::DerSignature = signing.sign(&info);
    let request_der = request(&info, signature.as_bytes());
    // Checked before anything leaves: the signature over the info verifies
    // with the key the request names.
    p256::ecdsa::VerifyingKey::from(&signing)
        .verify(&info, &signature)
        .map_err(|_| "the request did not verify")?;
    let hex = |s: &str| s.bytes().map(|b| format!("{b:02x}")).collect::<String>();
    if made {
        let d: [u8; 32] = secret.to_bytes().as_slice().try_into().map_err(|_| "a P-256 key of another length")?;
        println!("KEY {}", hex(&to_pem("EC PRIVATE KEY", &sec1_key(&d, &point))));
    }
    println!("CSR {}", hex(&to_pem("CERTIFICATE REQUEST", &request_der)));
    Ok(if made { "a new P-256 key and its request".into() } else { "a request for the key in secrets.rs".into() })
}
