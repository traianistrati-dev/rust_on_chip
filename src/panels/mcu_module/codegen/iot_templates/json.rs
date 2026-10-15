
// ── JSON: a value out of an answer ────────────────────────────────────────
// One value picked out of a JSON document by its path - AWS's shadow
// documents, any API's answer - walked in place: no heap, no recursion, no
// copy but a string's, so a document from anywhere can neither exhaust the
// stack nor panic the board.

/// The value at `path` in the JSON document `doc`, as its text: `21.5`,
/// `true`, `null`, `{"a":1}`, `"on"` (quoted, its escapes as sent). Each
/// step is an object's key, or an array's index ("0", "1", ...):
///
///     json_get(doc, &["state", "delta", "led"])
///
/// `None` when a step is not there, the objects and arrays the path goes
/// through are malformed (a missing `:` or `,`, a quote or bracket that
/// never closes), or the value runs past the end of `doc`: a body cut short
/// (`truncated`) loses what was cut, never gives half a value - but for a
/// document that is one bare number, read with an empty path, which ends
/// where a cut would: check `truncated` there. What the walk passes over is
/// matched for quotes and brackets only, never checked: this is no JSON
/// validator. A key that repeats: its first value.
#[allow(dead_code)]
pub fn json_get<'d>(doc: &'d [u8], path: &[&str]) -> Option<&'d [u8]> {
    let mut at = json_ws(doc, 0);
    for step in path {
        at = match *doc.get(at)? {
            b'{' => json_member(doc, at, step)?,
            b'[' => json_element(doc, at, step.parse().ok()?)?,
            _ => return None,
        };
    }
    let end = json_end(doc, at)?;
    // A number or a word that ends the bytes, inside a document, may have
    // been cut there: `21` of `21.5`.
    if end == doc.len() && !path.is_empty() && !matches!(doc[at], b'"' | b'{' | b'[') {
        return None;
    }
    Some(&doc[at..end])
}

/// `json_get`'s value as a number: `None` for anything but a JSON number,
/// or one beyond f64.
#[allow(dead_code)]
pub fn json_f64(doc: &[u8], path: &[&str]) -> Option<f64> {
    let v = json_get(doc, path)?;
    if !json_number(v) {
        return None;
    }
    let n: f64 = core::str::from_utf8(v).ok()?.parse().ok()?;
    n.is_finite().then_some(n)
}

/// `json_get`'s value as a whole number - no fraction, no exponent, within
/// i64: a shadow's `version`, a timestamp.
#[allow(dead_code)]
pub fn json_i64(doc: &[u8], path: &[&str]) -> Option<i64> {
    let v = json_get(doc, path)?;
    if !json_number(v) || v.iter().any(|b| matches!(b, b'.' | b'e' | b'E')) {
        return None;
    }
    core::str::from_utf8(v).ok()?.parse().ok()
}

/// `json_get`'s value as `true` or `false`.
#[allow(dead_code)]
pub fn json_bool(doc: &[u8], path: &[&str]) -> Option<bool> {
    match json_get(doc, path)? {
        b"true" => Some(true),
        b"false" => Some(false),
        _ => None,
    }
}

/// `json_get`'s value as a string, its escapes (`\n`, `\"`, `\u00e9`, a
/// surrogate pair) decoded into `out`. `None` for anything but a string, or
/// one that does not fit `out`.
#[allow(dead_code)]
pub fn json_str<'o>(doc: &[u8], path: &[&str], out: &'o mut [u8]) -> Option<&'o str> {
    let v = json_get(doc, path)?;
    if v.first() != Some(&b'"') {
        return None;
    }
    let mut n = 0;
    let whole = json_unescape(&v[1..v.len() - 1], |b| match out.get_mut(n) {
        Some(slot) => {
            *slot = b;
            n += 1;
            true
        }
        None => false,
    });
    if !whole {
        return None;
    }
    let out: &'o [u8] = out;
    core::str::from_utf8(&out[..n]).ok()
}

/// Past the whitespace at `i`.
#[allow(dead_code)]
fn json_ws(doc: &[u8], mut i: usize) -> usize {
    while doc.get(i).is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r')) {
        i += 1;
    }
    i
}

/// The end of the value at `at`, one past its last byte; `None` when it
/// runs past `doc`. A string to its closing quote, an object or an array to
/// the bracket that closes the first; a number or a word up to what ends a
/// value - so `1_000` is one word, no number, never `1`.
#[allow(dead_code)]
fn json_end(doc: &[u8], at: usize) -> Option<usize> {
    match *doc.get(at)? {
        b'"' => json_string_end(doc, at),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut i = at;
            while i < doc.len() {
                match doc[i] {
                    b'"' => {
                        i = json_string_end(doc, i)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            None
        }
        _ => {
            let n = doc[at..]
                .iter()
                .take_while(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b',' | b':' | b'"' | b'{' | b'}' | b'[' | b']'))
                .count();
            (n > 0).then_some(at + n)
        }
    }
}

/// One past the quote that closes the string opening at `at`.
#[allow(dead_code)]
fn json_string_end(doc: &[u8], at: usize) -> Option<usize> {
    let mut i = at + 1;
    while i < doc.len() {
        match doc[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            0..=0x1f => return None,
            _ => i += 1,
        }
    }
    None
}

/// Where the value of `key` starts, in the object opening at `at`.
#[allow(dead_code)]
fn json_member(doc: &[u8], at: usize, key: &str) -> Option<usize> {
    let mut i = json_ws(doc, at + 1);
    loop {
        if doc.get(i) != Some(&b'"') {
            return None;
        }
        let end = json_string_end(doc, i)?;
        let found = json_key_is(&doc[i + 1..end - 1], key);
        i = json_ws(doc, end);
        if doc.get(i) != Some(&b':') {
            return None;
        }
        i = json_ws(doc, i + 1);
        if found {
            return (i < doc.len()).then_some(i);
        }
        i = json_ws(doc, json_end(doc, i)?);
        if doc.get(i) != Some(&b',') {
            return None;
        }
        i = json_ws(doc, i + 1);
    }
}

/// Where element `index` starts, in the array opening at `at`.
#[allow(dead_code)]
fn json_element(doc: &[u8], at: usize, index: usize) -> Option<usize> {
    let mut i = json_ws(doc, at + 1);
    let mut n = 0;
    loop {
        if i >= doc.len() || doc[i] == b']' {
            return None;
        }
        if n == index {
            return Some(i);
        }
        i = json_ws(doc, json_end(doc, i)?);
        if doc.get(i) != Some(&b',') {
            return None;
        }
        i = json_ws(doc, i + 1);
        n += 1;
    }
}

/// A key as sent (between its quotes, escapes and all) is `key`.
#[allow(dead_code)]
fn json_key_is(raw: &[u8], key: &str) -> bool {
    let key = key.as_bytes();
    let mut k = 0;
    let whole = json_unescape(raw, |b| {
        let same = key.get(k) == Some(&b);
        k += 1;
        same
    });
    whole && k == key.len()
}

/// A string's bytes between its quotes, escapes decoded, handed to `put` one
/// by one; `false` at a bad escape, a lone surrogate, or when `put` says so.
#[allow(dead_code)]
fn json_unescape(raw: &[u8], mut put: impl FnMut(u8) -> bool) -> bool {
    let mut i = 0;
    while i < raw.len() {
        let b = raw[i];
        i += 1;
        if b != b'\\' {
            if !put(b) {
                return false;
            }
            continue;
        }
        let Some(&e) = raw.get(i) else {
            return false;
        };
        i += 1;
        let c = match e {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let Some(high) = json_hex4(raw, i) else {
                    return false;
                };
                i += 4;
                let code = if (0xD800..0xDC00).contains(&high) {
                    // A character past U+FFFF: its low half follows.
                    if raw.get(i..i + 2) != Some(b"\\u".as_slice()) {
                        return false;
                    }
                    let Some(low) = json_hex4(raw, i + 2).filter(|low| (0xDC00..0xE000).contains(low)) else {
                        return false;
                    };
                    i += 6;
                    0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
                } else {
                    high
                };
                // A low half alone is no character.
                let Some(c) = char::from_u32(code) else {
                    return false;
                };
                c
            }
            _ => return false,
        };
        let mut utf8 = [0u8; 4];
        for &u in c.encode_utf8(&mut utf8).as_bytes() {
            if !put(u) {
                return false;
            }
        }
    }
    true
}

/// Four hex digits at `at`.
#[allow(dead_code)]
fn json_hex4(raw: &[u8], at: usize) -> Option<u32> {
    let mut v = 0;
    for &d in raw.get(at..at + 4)? {
        v = v * 16 + char::from(d).to_digit(16)?;
    }
    Some(v)
}

/// RFC 8259's number: `-`, then `0` or digits not starting with `0`, an
/// optional fraction, an optional exponent - no `+1`, `.5`, `01`, `inf`.
#[allow(dead_code)]
fn json_number(v: &[u8]) -> bool {
    let digits = |i: &mut usize| {
        let from = *i;
        while v.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        *i - from
    };
    let mut i = usize::from(v.first() == Some(&b'-'));
    match v.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => {
            digits(&mut i);
        }
        _ => return false,
    }
    if v.get(i) == Some(&b'.') {
        i += 1;
        if digits(&mut i) == 0 {
            return false;
        }
    }
    if matches!(v.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(v.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return false;
        }
    }
    i == v.len()
}
