//! Small pure header / URL / base64 tools shared across the registry module: a case-insensitive header
//! lookup, relative-`Location` resolution against the registry origin, and a crate-free base64 decoder.

pub(in crate::registry) fn header(headers: &str, name: &str) -> Option<String> {
    let want = format!("{}:", name.to_ascii_lowercase());
    headers
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with(&want))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_string())
}
/// Resolve a possibly-relative `Location` against the registry base origin.
pub(in crate::registry) fn absolute(location: &str, base_v2: &str) -> String {
    if location.starts_with("http") {
        return location.to_string();
    }
    let origin = base_v2.split("/v2/").next().unwrap_or(base_v2);
    format!("{origin}{location}")
}

pub(in crate::registry) fn base64_decode(s: &str) -> Option<Vec<u8>> {
    // docker uses standard or URL-safe base64; do it without a crate
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut table = [255u8; 256];
    for (i, &c) in A.iter().enumerate() {
        table[c as usize] = i as u8;
    }
    table[b'-' as usize] = 62;
    table[b'_' as usize] = 63;
    let mut bits = 0u32;
    let mut nbits = 0;
    let mut out = Vec::new();
    for &c in s.as_bytes() {
        if c == b'=' || c == b'\n' || c == b'\r' {
            continue;
        }
        let v = table[c as usize];
        if v == 255 {
            return None;
        }
        bits = (bits << 6) | v as u32;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((bits >> nbits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- header: case-insensitive header lookup, trimmed value ----

    #[test]
    fn header_lookup_is_case_insensitive_and_trimmed() {
        let h = "Content-Type: text/plain\r\nDocker-Content-Digest: sha256:abc123\r\n";
        // name match ignores case; value is trimmed of the leading space.
        assert_eq!(header(h, "content-type"), Some("text/plain".to_string()));
        assert_eq!(header(h, "Content-Type"), Some("text/plain".to_string()));
        // a value containing a colon is preserved (split_once splits on the FIRST colon only).
        assert_eq!(
            header(h, "docker-content-digest"),
            Some("sha256:abc123".to_string())
        );
        // absent header -> None
        assert_eq!(header(h, "location"), None);
    }

    // ---- absolute: resolve a Location against the registry origin ----

    #[test]
    fn absolute_passes_through_absolute_urls() {
        assert_eq!(
            absolute("https://cdn.example.com/blob", "https://reg.example.com/v2/lib/ubuntu"),
            "https://cdn.example.com/blob"
        );
    }

    #[test]
    fn absolute_prepends_origin_for_relative() {
        // origin = everything before "/v2/"
        assert_eq!(
            absolute("/v2/lib/ubuntu/blobs/x", "https://reg.example.com/v2/lib/ubuntu"),
            "https://reg.example.com/v2/lib/ubuntu/blobs/x"
        );
        // base without "/v2/" -> the whole base is treated as the origin.
        assert_eq!(
            absolute("/path", "https://reg.example.com"),
            "https://reg.example.com/path"
        );
    }

    // ---- base64_decode: standard + URL-safe alphabets, no crate ----

    #[test]
    fn base64_decode_standard_with_padding() {
        // standard base64 of "foo:bar" (a docker registry basic-auth token shape)
        assert_eq!(base64_decode("Zm9vOmJhcg=="), Some(b"foo:bar".to_vec()));
    }

    #[test]
    fn base64_decode_no_padding_and_url_safe_alphabet() {
        // no '=' padding still decodes
        assert_eq!(base64_decode("aGVsbG8"), Some(b"hello".to_vec()));
        // URL-safe '-'/'_' map to 62/63: "-_8" -> [0xFB, 0xFF]
        assert_eq!(base64_decode("-_8"), Some(vec![0xFBu8, 0xFF]));
    }

    #[test]
    fn base64_decode_rejects_invalid_chars() {
        // a char outside both alphabets -> None
        assert_eq!(base64_decode("@@@"), None);
        // whitespace (\r/\n) inside the blob is skipped, not rejected
        assert_eq!(base64_decode("Zm9v\r\nOmJhcg=="), Some(b"foo:bar".to_vec()));
    }
}
