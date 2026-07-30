//! EGL dma-buf images shared between GBM, GLES, and the Wayland compositor.

use core::ffi::c_void;
use hl_surface_protocol::buffer::{Header, MODIFIER};
use std::collections::HashMap;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::fs::FileExt;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

pub const EGL_LINUX_DMA_BUF_EXT: u32 = 0x3270;
pub const EGL_NONE: isize = 0x3038;
pub const EGL_WIDTH: isize = 0x3057;
pub const EGL_HEIGHT: isize = 0x3056;
pub const EGL_LINUX_DRM_FOURCC_EXT: isize = 0x3271;
pub const EGL_DMA_BUF_PLANE0_FD_EXT: isize = 0x3272;
pub const EGL_DMA_BUF_PLANE0_OFFSET_EXT: isize = 0x3273;
pub const EGL_DMA_BUF_PLANE0_PITCH_EXT: isize = 0x3274;
pub const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: isize = 0x3443;
pub const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: isize = 0x3444;

pub const DRM_FORMAT_ARGB8888: u32 = u32::from_le_bytes(*b"AR24");
pub const DRM_FORMAT_XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;

#[derive(Debug)]
pub struct Image {
    pub fd: OwnedFd,
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub offset: u64,
    pub stride: u32,
    pub modifier: u64,
    pub(crate) external: Option<Header>,
}

impl Image {
    pub(crate) fn storage_key(&self) -> std::io::Result<StorageKey> {
        let metadata = std::fs::File::from(self.fd.try_clone()?).metadata()?;
        Ok(StorageKey {
            device: metadata.dev(),
            inode: metadata.ino(),
            width: self.width,
            height: self.height,
            fourcc: self.fourcc,
            offset: self.offset,
            stride: self.stride,
            modifier: self.modifier,
        })
    }

    /// Read the linear AR24/XR24 plane in its native little-endian BGRA byte order.
    ///
    /// `Bgra8Unorm` interprets these bytes as logical RGBA without a CPU channel swap. XRGB's unused alpha
    /// byte is canonicalized to opaque while the image is used as a GL texture.
    pub fn native_bgra(&self) -> std::io::Result<Vec<u8>> {
        if self.external.is_some() {
            return Err(std::io::Error::from(std::io::ErrorKind::Unsupported));
        }
        let row = usize::try_from(self.width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or_else(|| std::io::Error::other("dma-buf row size overflow"))?;
        let length = usize::try_from(self.height)
            .ok()
            .and_then(|height| height.checked_mul(row))
            .ok_or_else(|| std::io::Error::other("dma-buf image size overflow"))?;
        let mut bgra = vec![0; length];
        let file = std::fs::File::from(self.fd.try_clone()?);
        for y in 0..usize::try_from(self.height).unwrap_or(0) {
            let source = self
                .offset
                .checked_add(u64::from(self.stride).saturating_mul(y as u64))
                .ok_or_else(|| std::io::Error::other("dma-buf row offset overflow"))?;
            let destination = &mut bgra[y * row..(y + 1) * row];
            file.read_exact_at(destination, source)?;
            if self.fourcc == DRM_FORMAT_XRGB8888 {
                for pixel in destination.chunks_exact_mut(4) {
                    pixel[3] = 0xff;
                }
            }
        }
        Ok(bgra)
    }

    pub fn write_gl_rgba(&self, rgba: &[u8]) -> std::io::Result<()> {
        if self.external.is_some() {
            return Err(std::io::Error::from(std::io::ErrorKind::Unsupported));
        }
        let row = usize::try_from(self.width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or_else(|| std::io::Error::other("dma-buf row size overflow"))?;
        let expected = usize::try_from(self.height)
            .ok()
            .and_then(|height| height.checked_mul(row))
            .ok_or_else(|| std::io::Error::other("dma-buf image size overflow"))?;
        if rgba.len() != expected {
            return Err(std::io::Error::other(
                "rendered dma-buf plane has wrong size",
            ));
        }
        let file = std::fs::File::from(self.fd.try_clone()?);
        for destination_y in 0..usize::try_from(self.height).unwrap_or(0) {
            // glReadPixels is bottom-left; a linear Wayland dma-buf is top-left.
            let source_y = usize::try_from(self.height).unwrap_or(0) - 1 - destination_y;
            let mut row_pixels = rgba[source_y * row..(source_y + 1) * row].to_vec();
            for pixel in row_pixels.chunks_exact_mut(4) {
                pixel.swap(0, 2);
                if self.fourcc == DRM_FORMAT_XRGB8888 {
                    pixel[3] = 0;
                }
            }
            let destination = self
                .offset
                .checked_add(u64::from(self.stride).saturating_mul(destination_y as u64))
                .ok_or_else(|| std::io::Error::other("dma-buf row offset overflow"))?;
            file.write_all_at(&row_pixels, destination)?;
        }
        Ok(())
    }

    /// Write a renderer readback in native top-left BGRA order into this linear dma-buf.
    pub fn write_native_bgra(&self, bgra: &[u8]) -> std::io::Result<()> {
        if self.external.is_some() {
            return Err(std::io::Error::from(std::io::ErrorKind::Unsupported));
        }
        const MAX_STAGING_PLANE_BYTES: usize = 256 << 20;
        let row = usize::try_from(self.width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or_else(|| std::io::Error::other("dma-buf row size overflow"))?;
        let expected = usize::try_from(self.height)
            .ok()
            .and_then(|height| height.checked_mul(row))
            .ok_or_else(|| std::io::Error::other("dma-buf image size overflow"))?;
        if bgra.len() != expected {
            return Err(std::io::Error::other(
                "rendered dma-buf plane has wrong size",
            ));
        }
        let file = std::fs::File::from(self.fd.try_clone()?);
        let stride = usize::try_from(self.stride)
            .map_err(|_| std::io::Error::other("dma-buf stride is invalid"))?;
        if stride == row {
            if self.fourcc == DRM_FORMAT_ARGB8888 {
                return file.write_all_at(bgra, self.offset);
            }
            if expected > MAX_STAGING_PLANE_BYTES {
                return Err(std::io::Error::other(
                    "XRGB dma-buf plane exceeds writeback limit",
                ));
            }
            let mut plane = bgra.to_vec();
            for pixel in plane.chunks_exact_mut(4) {
                pixel[3] = 0;
            }
            return file.write_all_at(&plane, self.offset);
        }

        let height = usize::try_from(self.height)
            .map_err(|_| std::io::Error::other("dma-buf height is invalid"))?;
        let plane_len = stride
            .checked_mul(height)
            .ok_or_else(|| std::io::Error::other("dma-buf plane size overflow"))?;
        if plane_len > MAX_STAGING_PLANE_BYTES {
            return Err(std::io::Error::other(
                "padded dma-buf plane exceeds writeback limit",
            ));
        }
        // Preserve padding bytes while reducing a tall image from one syscall per row to one bounded read
        // and one write. Chromium's ordinary GBM planes are tight and take the allocation-free fast path.
        let mut plane = vec![0; plane_len];
        file.read_exact_at(&mut plane, self.offset)?;
        for y in 0..height {
            let source = &bgra[y * row..(y + 1) * row];
            let destination = &mut plane[y * stride..y * stride + row];
            destination.copy_from_slice(source);
            if self.fourcc == DRM_FORMAT_XRGB8888 {
                for pixel in destination.chunks_exact_mut(4) {
                    pixel[3] = 0;
                }
            }
        }
        file.write_all_at(&plane, self.offset)
    }

    pub fn external_token(&self) -> Option<hl_gpu::protocol::model::descriptor::SurfaceToken> {
        hl_gpu::protocol::model::descriptor::SurfaceToken::new(self.external?.token).ok()
    }

    pub fn publish(&self, serial: u64) -> std::io::Result<()> {
        self.external
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::Unsupported))?
            .with_serial(serial)
            .map_err(std::io::Error::other)?;
        let file = std::fs::File::from(self.fd.try_clone()?);
        let bytes = serial.to_le_bytes();
        let serial_offset = self
            .external
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::Unsupported))?
            .header_offset()
            .map_err(std::io::Error::other)?
            .checked_add(24)
            .ok_or_else(|| std::io::Error::other("external image serial offset overflow"))?;
        let mut written = 0;
        while written < bytes.len() {
            match file.write_at(&bytes[written..], serial_offset + written as u64) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "external image serial write made no progress",
                    ))
                }
                Ok(count) => written += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

/// Stable identity of one linear dma-buf plane. Descriptor integers are process-local aliases; file
/// identity plus complete plane layout is the shareable storage identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct StorageKey {
    device: u64,
    inode: u64,
    width: u32,
    height: u32,
    fourcc: u32,
    offset: u64,
    stride: u32,
    modifier: u64,
}

#[derive(Default)]
pub struct Images {
    next: usize,
    entries: HashMap<usize, Arc<Image>>,
    external_tokens: HashMap<u64, (Header, usize)>,
}

impl Images {
    pub fn import(
        &mut self,
        target: u32,
        attributes: *const isize,
        allow_external: bool,
    ) -> Option<*mut c_void> {
        let debug = std::env::var_os("HL_SHIM_DEBUG").is_some();
        if target != EGL_LINUX_DMA_BUF_EXT || attributes.is_null() {
            if debug {
                eprintln!(
                    "[hl-gl-shim] image-import reject target={target:#x} attributes_null={}",
                    attributes.is_null()
                );
            }
            return None;
        }
        let Some(attributes) = (unsafe { Attributes::parse(attributes) }) else {
            if debug {
                eprintln!(
                    "[hl-gl-shim] image-import reject target={target:#x} malformed_attributes"
                );
            }
            return None;
        };
        if debug {
            eprintln!(
                "[hl-gl-shim] image-import attrs target={target:#x} size={}x{} fourcc={:#x} fd={} \
                 offset={} stride={} modifier={:#x}",
                attributes.width,
                attributes.height,
                attributes.fourcc,
                attributes.fd,
                attributes.offset,
                attributes.stride,
                attributes.modifier
            );
        }
        if attributes.width == 0
            || attributes.height == 0
            || attributes.stride < attributes.width.saturating_mul(4)
            || attributes.fd < 0
            || !matches!(attributes.fourcc, DRM_FORMAT_ARGB8888 | DRM_FORMAT_XRGB8888)
            || !matches!(attributes.modifier, DRM_FORMAT_MOD_LINEAR | MODIFIER)
            || (attributes.modifier == MODIFIER && !allow_external)
        {
            if debug {
                eprintln!("[hl-gl-shim] image-import reject target={target:#x} unsupported_layout");
            }
            return None;
        }

        // SAFETY: the caller supplied a live dma-buf fd for the duration of eglCreateImage. Duplicating it
        // gives the EGLImage independent ownership, as required after the caller closes its descriptor.
        let fd = match unsafe { BorrowedFd::borrow_raw(attributes.fd) }.try_clone_to_owned() {
            Ok(fd) => fd,
            Err(error) => {
                if debug {
                    eprintln!(
                        "[hl-gl-shim] image-import reject target={target:#x} fd_clone={error}"
                    );
                }
                return None;
            }
        };
        let external = if attributes.modifier == MODIFIER {
            let mut bytes = [0; hl_surface_protocol::buffer::HEADER_LEN];
            let file = std::fs::File::from(fd.try_clone().ok()?);
            let header_offset = u64::from(attributes.stride)
                .checked_mul(u64::from(attributes.height))?
                .checked_add(attributes.offset)?;
            file.read_exact_at(&mut bytes, header_offset).ok()?;
            let header = Header::decode(&bytes).ok()?;
            let allocation_size = file.metadata().ok()?.len();
            let expected_allocation =
                header_offset.checked_add(hl_surface_protocol::buffer::HEADER_LEN as u64)?;
            if header.width != attributes.width
                || header.height != attributes.height
                || header.fourcc != attributes.fourcc
                || header.stride != attributes.stride
                || header.plane_offset != attributes.offset
                || header.plane_offset != hl_surface_protocol::buffer::PLANE_OFFSET
                || header.allocation_size != expected_allocation
                || allocation_size != expected_allocation
            {
                return None;
            }
            if let Some((canonical, _)) = self.external_tokens.get(&header.token) {
                let mut canonical = *canonical;
                let mut candidate = header;
                canonical.serial = 0;
                candidate.serial = 0;
                if canonical != candidate {
                    return None;
                }
            }
            Some(header)
        } else {
            None
        };
        self.next = self.next.saturating_add(1).max(1);
        let token = self.next;
        self.entries.insert(
            token,
            Arc::new(Image {
                fd,
                width: attributes.width,
                height: attributes.height,
                fourcc: attributes.fourcc,
                offset: attributes.offset,
                stride: attributes.stride,
                modifier: attributes.modifier,
                external,
            }),
        );
        if let Some(header) = external {
            self.external_tokens
                .entry(header.token)
                .and_modify(|(_, references)| *references += 1)
                .or_insert((header, 1));
        }
        if debug {
            eprintln!("[hl-gl-shim] image-import success target={target:#x} image={token:#x}");
        }
        Some(token as *mut c_void)
    }

    /// Import through the `EGL_KHR_image_base` ABI, whose attribute list uses 32-bit `EGLint`
    /// entries even on 64-bit processes. `eglCreateImage` uses pointer-sized `EGLAttrib` instead.
    pub unsafe fn import_khr(
        &mut self,
        target: u32,
        mut attributes: *const i32,
        allow_external: bool,
    ) -> Option<*mut c_void> {
        if attributes.is_null() {
            return None;
        }
        let mut widened = Vec::with_capacity(17);
        for _ in 0..64 {
            let key = unsafe { *attributes };
            widened.push(key as isize);
            if key == EGL_NONE as i32 {
                return self.import(target, widened.as_ptr(), allow_external);
            }
            widened.push(unsafe { *attributes.add(1) } as isize);
            attributes = unsafe { attributes.add(2) };
        }
        None
    }

    pub fn get(&self, token: *mut c_void) -> Option<Arc<Image>> {
        self.entries.get(&(token as usize)).cloned()
    }

    pub fn remove(&mut self, token: *mut c_void) -> bool {
        let token = token as usize;
        let Some(image) = self.entries.remove(&token) else {
            return false;
        };
        if let Some(header) = image.external {
            let Some((_, references)) = self.external_tokens.get_mut(&header.token) else {
                return true;
            };
            *references -= 1;
            if *references == 0 {
                self.external_tokens.remove(&header.token);
            }
        }
        true
    }
}

struct Attributes {
    width: u32,
    height: u32,
    fourcc: u32,
    fd: i32,
    offset: u64,
    stride: u32,
    modifier: u64,
}

impl Attributes {
    unsafe fn parse(mut values: *const isize) -> Option<Self> {
        let mut width = None;
        let mut height = None;
        let mut fourcc = None;
        let mut fd = None;
        let mut offset = 0;
        let mut stride = None;
        let mut modifier_lo = 0u32;
        let mut modifier_hi = 0u32;

        // EGL attribute lists are terminated key/value pairs. Bound the walk so malformed guest input
        // cannot make the injected shim scan arbitrary memory indefinitely.
        for _ in 0..64 {
            let key = unsafe { *values };
            if key == EGL_NONE {
                return Some(Self {
                    width: width?,
                    height: height?,
                    fourcc: fourcc?,
                    fd: fd?,
                    offset,
                    stride: stride?,
                    modifier: u64::from(modifier_lo) | (u64::from(modifier_hi) << 32),
                });
            }
            let value = unsafe { *values.add(1) };
            match key {
                EGL_WIDTH => width = u32::try_from(value).ok(),
                EGL_HEIGHT => height = u32::try_from(value).ok(),
                EGL_LINUX_DRM_FOURCC_EXT => fourcc = u32::try_from(value).ok(),
                EGL_DMA_BUF_PLANE0_FD_EXT => fd = i32::try_from(value).ok(),
                EGL_DMA_BUF_PLANE0_OFFSET_EXT => offset = u64::try_from(value).ok()?,
                EGL_DMA_BUF_PLANE0_PITCH_EXT => stride = u32::try_from(value).ok(),
                EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT => modifier_lo = value as u32,
                EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT => modifier_hi = value as u32,
                _ => return None,
            }
            values = unsafe { values.add(2) };
        }
        None
    }
}

#[cfg(test)]
#[path = "image/tests.rs"]
mod tests;
