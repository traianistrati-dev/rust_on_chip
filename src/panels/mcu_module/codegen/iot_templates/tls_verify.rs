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
