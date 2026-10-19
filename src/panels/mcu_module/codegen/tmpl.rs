//! The generator's templates: whole config files kept as FILES under
//! `codegen/templates/<stack>/<unit>[_<variant>].rs.tmpl`, one directory per
//! HAL stack (`f1_hal`, `embassy_stm32`, `esp_hal`, `rp_hal`, `embassy_rp`,
//! `nrf_hal`, `embassy_nrf`, `common`), and the one way they become text.
//!
//! A template is the generated file byte for byte, with `{UPPER_SNAKE}`
//! tokens where the model's values go. `.rs.tmpl`, not `.rs`: most of them
//! carry tokens below the GENERATED block too (a signature that follows the
//! wiring), so they are not Rust until rendered, and neither rustfmt nor
//! rust-analyzer may treat them as source. They are compiled in with
//! `include_str!` - never read at run time, so the app and the matrix always
//! render the same bytes - and checked out LF by `.gitattributes`.

/// `template` with each `(token, value)` substituted IN ORDER, every
/// occurrence, as a `.replace` chain would: a value may carry a token that a
/// later pair fills (an example block that names the handle, say), so the
/// order is part of the template's meaning.
///
/// Under test, a `{UPPER_SNAKE}` left in the result panics: a token with no
/// value is a bug in the generator, and the project it lands in does not
/// compile. In the app it stays text - a broken file the user can see beats
/// an IDE that panics while they wire a pin.
pub fn render(template: &str, subs: &[(&str, &str)]) -> String {
    let mut out = template.to_owned();
    for (token, value) in subs {
        debug_assert!(is_token(token), "not a {{TOKEN}}: {token}");
        out = out.replace(token, value);
    }
    if cfg!(test) {
        if let Some(left) = leftover(&out) {
            panic!("{left} was never filled in:\n{out}");
        }
    }
    out
}

/// `template` with each token replaced ONCE, in one pass over the template:
/// a value is never searched for tokens again. This is `format!`'s meaning -
/// for the templates that started life as a `format!` body, whose values are
/// generated code that may hold anything.
pub fn fill(template: &str, subs: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len() * 2);
    let mut rest = template;
    while let Some(at) = rest.find('{') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        match tail.find('}').map(|end| &tail[..=end]) {
            Some(token) if is_token(token) => {
                match subs.iter().find(|(t, _)| *t == token) {
                    Some((_, value)) => out.push_str(value),
                    None if cfg!(test) => panic!("{token} has no value:\n{template}"),
                    None => out.push_str(token),
                }
                rest = &tail[token.len()..];
            }
            _ => {
                out.push('{');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The first `{UPPER_SNAKE}` token in `text`, if one is left.
///
/// The shape is the whole rule: none of the 2106 files the emit harnesses
/// wrote on 2026-10-18 contains one, while Rust's own braces never match it
/// (`{}`, `{:?}`, `{ x }`, `{n}`).
pub fn leftover(text: &str) -> Option<&str> {
    let mut rest = text;
    while let Some(at) = rest.find('{') {
        let tail = &rest[at..];
        if let Some(end) = tail.find('}') {
            if is_token(&tail[..=end]) {
                return Some(&tail[..=end]);
            }
        }
        rest = &tail[1..];
    }
    None
}

fn is_token(s: &str) -> bool {
    let Some(name) = s.strip_prefix('{').and_then(|s| s.strip_suffix('}')) else {
        return false;
    };
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_substitutes_in_order() {
        // The example names the handle; the handle is filled after it.
        let t = "{EXAMPLE}\nlet {HANDLE} = init();\n";
        let out = render(t, &[("{EXAMPLE}", "// use {HANDLE}"), ("{HANDLE}", "_serial1")]);
        assert_eq!(out, "// use _serial1\nlet _serial1 = init();\n");
    }

    #[test]
    #[should_panic(expected = "{BAUD} was never filled in")]
    fn render_refuses_a_token_left_behind() {
        render("const BAUD: u32 = {BAUD};\n", &[("{N}", "1")]);
    }

    #[test]
    fn fill_never_searches_a_value_again() {
        let out = fill("a {X} b {Y}", &[("{X}", "{Y}"), ("{Y}", "2")]);
        assert_eq!(out, "a {Y} b 2");
    }

    #[test]
    #[should_panic(expected = "{Y} has no value")]
    fn fill_refuses_a_token_without_a_value() {
        fill("{X} {Y}", &[("{X}", "1")]);
    }

    #[test]
    fn rust_braces_are_not_tokens() {
        for t in ["{}", "{:?}", "{ x }", "{n}", "{X:?}", "{0}", "Config { N }", "{_A}", "{}}"] {
            assert_eq!(leftover(t), None, "{t}");
        }
        assert_eq!(leftover("x {DMA_TX} y"), Some("{DMA_TX}"));
        assert_eq!(fill("{ x } {} {:?}", &[]), "{ x } {} {:?}");
    }
}
