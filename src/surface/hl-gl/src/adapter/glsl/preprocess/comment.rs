//! GLSL ES 1.00 §3.3 comment removal — translation phase 2, the input to preprocessing.

use crate::adapter::glsl::Source;

/// Strip `//` and `/* */` comments (gl_shim.c `strip_comments`).
impl Source<'_> {
    pub(crate) fn comments_removed(self) -> String {
        let b = self.text.as_bytes();
        let n = b.len();
        let mut out = String::with_capacity(n);
        let mut r = 0;
        let mut quote = None;
        while r < n {
            if let Some(delimiter) = quote {
                out.push(b[r] as char);
                if b[r] == b'\\' && r + 1 < n {
                    r += 1;
                    out.push(b[r] as char);
                } else if b[r] == delimiter {
                    quote = None;
                }
                r += 1;
            } else if b[r] == b'\'' || b[r] == b'"' {
                quote = Some(b[r]);
                out.push(b[r] as char);
                r += 1;
            } else if r + 1 < n && b[r] == b'/' && b[r + 1] == b'/' {
                while r < n && b[r] != b'\n' {
                    r += 1;
                }
            } else if r + 1 < n && b[r] == b'/' && b[r + 1] == b'*' {
                // GLSL ES 1.00 §3.3: a comment is replaced by a single space, and the newlines it spans are
                // KEPT — the preprocessor is line-based, so swallowing them would join a directive to the
                // following line and shift every reported line number.
                out.push(' ');
                r += 2;
                let mut newlines = 0usize;
                while r + 1 < n && !(b[r] == b'*' && b[r + 1] == b'/') {
                    if b[r] == b'\n' {
                        newlines += 1;
                    }
                    r += 1;
                }
                if r + 1 < n {
                    r += 2;
                } else {
                    r = n;
                }
                for _ in 0..newlines {
                    out.push('\n');
                }
            } else {
                out.push(b[r] as char);
                r += 1;
            }
        }
        out
    }
}
