
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
