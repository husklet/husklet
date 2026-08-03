//! Identifier scanning over a GLSL source fragment (GLSL ES 1.00 §3.7).

/// A source fragment being scanned for identifiers. Every byte inspected is ASCII, and a UTF-8 continuation
/// byte never is, so every offset this type produces or accepts is a char boundary — a shader carrying
/// non-ASCII bytes cannot make a slice panic.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Words<'a>(pub(crate) &'a str);

impl<'a> Words<'a> {
    /// End of the preprocessing-number token beginning at `at`. Identifiers embedded in such a token are
    /// not separate macro-expansion candidates: preprocessing-token formation precedes macro replacement.
    pub(crate) fn number_end(&self, at: usize) -> Option<usize> {
        let bytes = self.0.as_bytes();
        let first = *bytes.get(at)?;
        if !first.is_ascii_digit()
            && !(first == b'.' && bytes.get(at + 1).is_some_and(u8::is_ascii_digit))
        {
            return None;
        }
        let mut end = at + 1;
        while end < bytes.len() {
            let byte = bytes[end];
            if byte == b'_' || byte == b'.' || byte.is_ascii_alphanumeric() {
                end += 1;
            } else if matches!(byte, b'+' | b'-')
                && matches!(bytes.get(end - 1), Some(b'e' | b'E' | b'p' | b'P'))
            {
                end += 1;
            } else {
                break;
            }
        }
        Some(end)
    }

    /// The identifier starting exactly at `at`, or `None` when the byte there does not begin one.
    pub(crate) fn at(&self, at: usize) -> Option<&'a str> {
        let bytes = self.0.as_bytes();
        let first = *bytes.get(at)?;
        if first != b'_' && !first.is_ascii_alphabetic() {
            return None;
        }
        let mut end = at;
        while end < bytes.len() && Self::is_continuation(bytes[end]) {
            end += 1;
        }
        Some(&self.0[at..end])
    }

    /// The whole fragment when it is exactly one identifier.
    pub(crate) fn whole(&self) -> Option<&'a str> {
        self.at(0).filter(|word| word.len() == self.0.len())
    }

    /// Copy the single character at `at` — which does not begin an identifier — and return the next offset.
    pub(crate) fn copy(&self, at: usize, out: &mut String) -> usize {
        match self.0[at..].chars().next() {
            Some(character) => {
                out.push(character);
                at + character.len_utf8()
            }
            None => self.0.len(),
        }
    }

    pub(crate) fn is_continuation(byte: u8) -> bool {
        byte == b'_' || byte.is_ascii_alphanumeric()
    }
}
