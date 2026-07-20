//! URL-encoding helper + (in a later task) the generic `call()` the generated
//! client fns delegate to.

/// Percent-encode one path segment. Mirrors the TS runtime's
/// `encodeURIComponent` (slightly stricter — also encodes `-`, `.`, `_`, `~` —
/// which is safe: the server percent-decodes either way).
pub fn urlencode(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencode_escapes_separators_and_spaces() {
        assert_eq!(urlencode("a/b c"), "a%2Fb%20c");
        assert_eq!(urlencode("plain42"), "plain42");
    }
}
