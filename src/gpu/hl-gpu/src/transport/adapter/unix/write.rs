use std::io::{self, Write};

#[derive(Debug)]
pub struct WriteFailure {
    pub error: io::Error,
    pub accepted: usize,
}

pub(super) fn tracked(
    stream: &mut impl Write,
    mut bytes: &[u8],
    accepted: &mut usize,
) -> Result<(), WriteFailure> {
    while !bytes.is_empty() {
        match stream.write(bytes) {
            Ok(0) => {
                return Err(WriteFailure {
                    error: io::Error::new(io::ErrorKind::WriteZero, "failed to write frame"),
                    accepted: *accepted,
                });
            }
            Ok(written) => {
                *accepted += written;
                bytes = &bytes[written..];
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                return Err(WriteFailure {
                    error,
                    accepted: *accepted,
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Failure {
        accepted: usize,
        kind: io::ErrorKind,
    }

    impl Write for Failure {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.accepted == 0 {
                return Err(io::Error::from(self.kind));
            }
            let accepted = self.accepted.min(bytes.len());
            self.accepted = 0;
            Ok(accepted)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn failure_before_acceptance_is_certain() {
        let failure = tracked(
            &mut Failure {
                accepted: 0,
                kind: io::ErrorKind::BrokenPipe,
            },
            &[1, 2, 3],
            &mut 0,
        )
        .unwrap_err();
        assert_eq!(failure.accepted, 0);
    }

    #[test]
    fn failure_after_partial_acceptance_is_ambiguous() {
        let failure = tracked(
            &mut Failure {
                accepted: 2,
                kind: io::ErrorKind::BrokenPipe,
            },
            &[1, 2, 3],
            &mut 0,
        )
        .unwrap_err();
        assert_eq!(failure.accepted, 2);
    }
}
