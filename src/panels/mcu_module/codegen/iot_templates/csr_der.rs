// ── A P-256 key and its certificate request, as DER ───────────────────────
// No cryptography here: the "Test from this PC" helper signs, this lays the
// bytes out. Shared with the IDE, whose tests check the layout.

/// One DER element: `tag`, its length (long form past 127), `content`.
pub fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = content.len();
    if len < 0x80 {
        out.push(len as u8);
    } else if len < 0x100 {
        out.extend_from_slice(&[0x81, len as u8]);
    } else {
        out.extend_from_slice(&[0x82, (len >> 8) as u8, len as u8]);
    }
    out.extend_from_slice(content);
    out
}

const OID_COUNTRY: [u8; 3] = [0x55, 0x04, 0x06];
const OID_ORGANIZATION: [u8; 3] = [0x55, 0x04, 0x0A];
const OID_COMMON_NAME: [u8; 3] = [0x55, 0x04, 0x03];
const OID_EC_PUBLIC_KEY: [u8; 7] = [0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];
const OID_PRIME256V1: [u8; 8] = [0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07];
const OID_ECDSA_SHA256: [u8; 8] = [0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x02];

/// Who the certificate is for. `country`: two letters or empty; empty parts
/// are left out (AWS IoT needs none; test.mosquitto.org wants C, O and CN).
pub struct Subject<'a> {
    pub country: &'a str,
    pub organization: &'a str,
    pub common_name: &'a str,
}

impl Subject<'_> {
    /// What is wrong with it, for the tab.
    pub fn problem(&self) -> Option<&'static str> {
        if !self.country.is_empty() && (self.country.len() != 2 || !self.country.bytes().all(|b| b.is_ascii_alphabetic())) {
            return Some("the country is two letters (RO, DE, US) or nothing");
        }
        if self.common_name.trim().is_empty() {
            return Some("the common name (CN) is needed - the client id, say");
        }
        if self.common_name.len() > 64 || self.organization.len() > 64 {
            return Some("a name longer than 64 bytes");
        }
        None
    }

    /// The X.501 Name: C as PrintableString, O and CN as UTF8String.
    pub fn name(&self) -> Vec<u8> {
        let mut rdns = Vec::new();
        for (oid, tag, value) in [
            (&OID_COUNTRY[..], 0x13, self.country.to_ascii_uppercase()),
            (&OID_ORGANIZATION[..], 0x0C, self.organization.trim().to_owned()),
            (&OID_COMMON_NAME[..], 0x0C, self.common_name.trim().to_owned()),
        ] {
            if value.is_empty() {
                continue;
            }
            let attribute = der(0x30, &[der(0x06, oid), der(tag, value.as_bytes())].concat());
            rdns.extend(der(0x31, &attribute));
        }
        der(0x30, &rdns)
    }
}

/// A P-256 public key as SubjectPublicKeyInfo: `point` is `04 X Y`.
pub fn spki(point: &[u8; 65]) -> Vec<u8> {
    let algorithm = der(0x30, &[der(0x06, &OID_EC_PUBLIC_KEY), der(0x06, &OID_PRIME256V1)].concat());
    let mut bits = vec![0u8];
    bits.extend_from_slice(point);
    der(0x30, &[algorithm, der(0x03, &bits)].concat())
}

/// PKCS#10 CertificationRequestInfo: version 0, the subject, the key, no
/// attributes (`[0]` empty - it must be there).
pub fn request_info(subject: &Subject, point: &[u8; 65]) -> Vec<u8> {
    let version = der(0x02, &[0]);
    der(0x30, &[version, subject.name(), spki(point), vec![0xA0, 0x00]].concat())
}

/// The CertificationRequest: the info, ecdsa-with-SHA256 (no parameters,
/// RFC 5758), and `signature` - DER (r, s) - over the info's DER.
pub fn request(info: &[u8], signature: &[u8]) -> Vec<u8> {
    let algorithm = der(0x30, &der(0x06, &OID_ECDSA_SHA256));
    let mut bits = vec![0u8];
    bits.extend_from_slice(signature);
    der(0x30, &[info.to_vec(), algorithm, der(0x03, &bits)].concat())
}

/// A SEC1 ECPrivateKey (`EC PRIVATE KEY`): the scalar, the curve, and the
/// public point - so the IDE can tell, without any cryptography, whether a
/// certificate belongs to it.
pub fn sec1_key(d: &[u8; 32], point: &[u8; 65]) -> Vec<u8> {
    let mut bits = vec![0u8];
    bits.extend_from_slice(point);
    der(
        0x30,
        &[
            der(0x02, &[1]),
            der(0x04, d),
            der(0xA0, &der(0x06, &OID_PRIME256V1)),
            der(0xA1, &der(0x03, &bits)),
        ]
        .concat(),
    )
}

/// `der` as PEM under `label`, 64 characters a line.
pub fn to_pem(label: &str, der: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut b64 = String::new();
    for chunk in der.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                b64.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                b64.push('=');
            }
        }
    }
    let mut out = format!("-----BEGIN {label}-----\n");
    for line in b64.as_bytes().chunks(64) {
        out.push_str(&String::from_utf8_lossy(line));
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}
