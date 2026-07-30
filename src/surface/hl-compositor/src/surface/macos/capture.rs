use std::fs;
use std::io;
use std::path::PathBuf;

use crate::scene::model::SurfaceId;

/// Persistent diagnostic capture of the latest composited frame for each native surface.
pub(super) struct Capture {
    directory: PathBuf,
    request: Option<PathBuf>,
}

impl Capture {
    pub(super) fn new(directory: impl Into<PathBuf>) -> io::Result<Self> {
        let directory = directory.into();
        fs::create_dir_all(&directory)?;
        Ok(Self {
            directory,
            request: None,
        })
    }

    pub(super) fn requested(directory: impl Into<PathBuf>) -> io::Result<Self> {
        let directory = directory.into();
        fs::create_dir_all(&directory)?;
        Ok(Self {
            request: Some(directory.join("request")),
            directory,
        })
    }

    pub(super) fn pending(&self) -> bool {
        self.request
            .as_ref()
            .is_none_or(|request| request.is_file())
    }

    pub(super) fn claim(&self) -> bool {
        let Some(request) = &self.request else {
            return true;
        };
        if !request.is_file() {
            return false;
        }
        let claimed = request.with_extension("claimed");
        let _ = fs::remove_file(&claimed);
        fs::rename(request, claimed).is_ok()
    }

    pub(super) fn write(
        &self,
        surface: SurfaceId,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> io::Result<()> {
        let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
        ppm.extend(
            rgba.chunks_exact(4)
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]),
        );

        let path = self.destination(surface);
        let pending = path.with_extension("ppm.pending");
        fs::write(&pending, ppm)?;
        fs::rename(pending, path)
    }

    fn destination(&self, surface: SurfaceId) -> PathBuf {
        self.directory.join(format!("surface-{}.ppm", surface.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_surface_frame_is_replaced_atomically() {
        let directory = std::env::temp_dir().join(format!("hl-mac-capture-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let capture = Capture::new(&directory).unwrap();

        capture
            .write(SurfaceId(7), 2, 1, &[1, 2, 3, 255, 4, 5, 6, 128])
            .unwrap();
        assert_eq!(
            fs::read(directory.join("surface-7.ppm")).unwrap(),
            b"P6\n2 1\n255\n\x01\x02\x03\x04\x05\x06"
        );

        capture.write(SurfaceId(7), 1, 1, &[9, 8, 7, 255]).unwrap();
        assert_eq!(
            fs::read(directory.join("surface-7.ppm")).unwrap(),
            b"P6\n1 1\n255\n\x09\x08\x07"
        );
        assert!(!directory.join("surface-7.ppm.pending").exists());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn requested_capture_claims_exactly_one_frame() {
        let directory =
            std::env::temp_dir().join(format!("hl-mac-capture-request-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let capture = Capture::requested(&directory).unwrap();

        assert!(!capture.pending());
        assert!(!capture.claim());
        fs::write(directory.join("request"), []).unwrap();
        assert!(capture.pending());
        assert!(capture.claim());
        assert!(!capture.pending());
        assert!(!capture.claim());
        assert!(directory.join("request.claimed").is_file());
        let _ = fs::remove_dir_all(directory);
    }
}
