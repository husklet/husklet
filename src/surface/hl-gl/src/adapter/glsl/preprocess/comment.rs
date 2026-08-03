//! GLSL ES 1.00 §3.3 comment removal — translation phase 2, the input to preprocessing.

use crate::adapter::glsl::Source;

/// Strip `//` and `/* */` comments (gl_shim.c `strip_comments`).
impl Source<'_> {
    pub(super) fn unterminated_block_comment(self) -> Option<usize> {
        let bytes = self.text.as_bytes();
        let mut at = 0usize;
        let mut line = 1usize;
        let mut quote = None;
        while at < bytes.len() {
            if let Some(delimiter) = quote {
                if bytes[at] == b'\\' && at + 1 < bytes.len() {
                    at += 2;
                } else {
                    if bytes[at] == delimiter {
                        quote = None;
                    }
                    if bytes[at] == b'\n' {
                        line += 1;
                    }
                    at += 1;
                }
            } else if bytes[at] == b'\'' || bytes[at] == b'"' {
                quote = Some(bytes[at]);
                at += 1;
            } else if bytes[at] == b'\n' {
                line += 1;
                at += 1;
            } else if at + 1 < bytes.len() && bytes[at] == b'/' && bytes[at + 1] == b'/' {
                at += 2;
                while at < bytes.len() && bytes[at] != b'\n' {
                    at += 1;
                }
            } else if at + 1 < bytes.len() && bytes[at] == b'/' && bytes[at + 1] == b'*' {
                let opening_line = line;
                at += 2;
                loop {
                    if at + 1 >= bytes.len() {
                        return Some(opening_line);
                    }
                    if bytes[at] == b'*' && bytes[at + 1] == b'/' {
                        at += 2;
                        break;
                    }
                    if bytes[at] == b'\n' {
                        line += 1;
                    }
                    at += 1;
                }
            } else {
                at += 1;
            }
        }
        None
    }

    pub(crate) fn comments_removed(self) -> String {
        self.remove_comments(false)
    }

    /// Replace a multi-line comment with one logical space while retaining its physical line count. The
    /// internal line splices are consumed by `LogicalLines`; they never reach the translated shader.
    pub(super) fn comments_removed_for_preprocessing(self) -> String {
        self.remove_comments(true)
    }

    fn remove_comments(self, splice_comment_lines: bool) -> String {
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
                    if splice_comment_lines {
                        out.push('\\');
                    }
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
