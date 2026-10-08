// ── The broker's certificate ──────────────────────────────────────────────
// Everything from here down is shared, as it is, with the IoT tab's "Test
// from this PC", which runs the same checks on the PC against the same broker
// - full paths, so it stands in either file. It uses CertClock and MAX_CHAIN.

/// embedded-tls's `CertVerifier` (each certificate's signature by the next,
/// the last by CA_DER, the name, the dates when CertClock knows the time),
/// plus the checks it leaves out: every certificate after the broker's own
/// must be a CA - without it, a chain [any name <- an ordinary certificate
/// someone bought <- ... <- CA_DER] is accepted - and an RSA-PSS signature
/// only from an RSA key: embedded-tls 0.19 unwraps the key as RSA (pki.rs:214),
/// and a replayed public EC chain would panic it there.
struct Verifier<'a> {
    inner: embedded_tls::pki::CertVerifier<'a, embedded_tls::Aes128GcmSha256, CertClock, MAX_CHAIN>,
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
            inner: embedded_tls::pki::CertVerifier::new(embedded_tls::Certificate::X509(ca)),
            chain_ok: false,
            leaf_rsa: false,
            authenticated: false,
        }
    }
}

impl embedded_tls::TlsVerifier<embedded_tls::Aes128GcmSha256> for Verifier<'_> {
    fn set_hostname_verification(&mut self, hostname: &str) -> Result<(), embedded_tls::TlsError> {
        self.inner.set_hostname_verification(hostname)
    }

    fn verify_certificate(
        &mut self,
        transcript: &embedded_tls::Sha256,
        chain: embedded_tls::CertificateRef,
    ) -> Result<(), embedded_tls::TlsError> {
        for issuer in chain.entries.iter().skip(1) {
            match issuer {
                embedded_tls::CertificateEntryRef::X509(der) if is_ca(der) => {}
                _ => return Err(embedded_tls::TlsError::InvalidCertificate),
            }
        }
        let leaf_rsa = matches!(chain.entries.first(), Some(embedded_tls::CertificateEntryRef::X509(der)) if key_is_rsa(der));
        self.inner.verify_certificate(transcript, chain)?;
        self.leaf_rsa = leaf_rsa;
        self.chain_ok = true;
        Ok(())
    }

    fn verify_signature(
        &mut self,
        verify: embedded_tls::CertificateVerifyRef,
    ) -> Result<(), embedded_tls::TlsError> {
        // Without a checked chain first, embedded-tls 0.19 would panic here.
        if !self.chain_ok {
            return Err(embedded_tls::TlsError::InvalidCertificate);
        }
        let pss = matches!(
            verify.signature_scheme,
            embedded_tls::SignatureScheme::RsaPssRsaeSha256
                | embedded_tls::SignatureScheme::RsaPssRsaeSha384
                | embedded_tls::SignatureScheme::RsaPssRsaeSha512
        );
        if pss && !self.leaf_rsa {
            return Err(embedded_tls::TlsError::InvalidSignatureScheme);
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
