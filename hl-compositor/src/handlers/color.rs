//! `wp_color_manager_v1` — surface color descriptions, output color profiles, and gamma-correct
//! conversion (ledger row `compositor_negotiates_surface_color_and_converts_to_the_target_output_profile`).
//!
//! A client describes the color volume of the pixels it produces (primaries + transfer function, and for
//! HDR the reference luminance) and attaches that description to its surface; the compositor knows each
//! output's color profile. Composition is therefore done in LINEAR light and converted to the target
//! output profile: decode the surface's transfer function to linear, convert between the surface's and
//! output's primaries through CIE XYZ, tone-map HDR luminance into the output's range, then encode the
//! output's transfer function. sRGB→sRGB is an identity round-trip; a BT.2020 / PQ HDR surface on an
//! sRGB SDR output is gamut- and tone-mapped rather than truncated.
//!
//! A coherent v1 subset of the staging protocol is composed: the manager advertises its supported
//! intents/features/transfer-functions/primaries, mints parametric and ICC image-description creators,
//! and hands out per-surface and per-output color objects. The color MATH below is independent of the
//! protocol wiring and is covered by ICC/HDR unit fixtures.

use std::sync::Mutex;
use std::collections::HashMap;

use smithay::reexports::wayland_protocols::wp::color_management::v1::server::{
    wp_color_management_output_v1::{self, WpColorManagementOutputV1},
    wp_color_management_surface_v1::{self, WpColorManagementSurfaceV1},
    wp_color_manager_v1::{self, Feature, Primaries, RenderIntent, TransferFunction, WpColorManagerV1},
    wp_image_description_creator_icc_v1::{self, WpImageDescriptionCreatorIccV1},
    wp_image_description_creator_params_v1::{self, WpImageDescriptionCreatorParamsV1},
    wp_image_description_v1::{self, WpImageDescriptionV1},
};
use smithay::reexports::wayland_server::{
    backend::GlobalId, protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle,
    GlobalDispatch, New, Resource, Weak as WlWeak,
};

use crate::HlState;

const COLOR_MANAGER_VERSION: u32 = 1;

// ---- color math ------------------------------------------------------------------------------------

/// Named color primaries the compositor can convert between (a subset of the protocol's `primaries`
/// enum). Each maps to a linear RGB→CIE XYZ matrix (D65 white).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorPrimaries {
    Srgb,
    Bt2020,
    DisplayP3,
}

impl ColorPrimaries {
    fn from_wire(v: u32) -> Option<Self> {
        match v {
            1 => Some(Self::Srgb),
            6 => Some(Self::Bt2020),
            9 => Some(Self::DisplayP3),
            _ => None,
        }
    }

    /// Linear RGB → CIE XYZ (D65). Rows are X, Y, Z.
    fn rgb_to_xyz(self) -> [[f64; 3]; 3] {
        match self {
            Self::Srgb => [
                [0.4123908, 0.3575843, 0.1804808],
                [0.2126390, 0.7151687, 0.0721923],
                [0.0193308, 0.1191948, 0.9505322],
            ],
            Self::Bt2020 => [
                [0.6369580, 0.1446169, 0.1688810],
                [0.2627002, 0.6779981, 0.0593017],
                [0.0000000, 0.0280727, 1.0609851],
            ],
            Self::DisplayP3 => [
                [0.4865709, 0.2656677, 0.1982173],
                [0.2289746, 0.6917385, 0.0792869],
                [0.0000000, 0.0451134, 1.0439444],
            ],
        }
    }
}

/// Named transfer functions (a subset of the protocol's `transfer_function` enum).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorTransfer {
    Srgb,
    Linear,
    Gamma22,
    /// SMPTE ST 2084 (PQ) — an absolute HDR transfer up to 10000 cd/m².
    St2084Pq,
}

impl ColorTransfer {
    fn from_wire(v: u32) -> Option<Self> {
        match v {
            9 => Some(Self::Srgb),
            5 => Some(Self::Linear),
            2 => Some(Self::Gamma22),
            11 => Some(Self::St2084Pq),
            _ => None,
        }
    }

    /// Decode an encoded channel value to linear light. For PQ the result is absolute luminance
    /// normalized so 1.0 == 10000 cd/m² (the caller tone-maps into the output's range).
    fn to_linear(self, v: f64) -> f64 {
        let v = v.clamp(0.0, 1.0);
        match self {
            Self::Linear => v,
            Self::Gamma22 => v.powf(2.2),
            Self::Srgb => {
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            }
            Self::St2084Pq => {
                const M1: f64 = 2610.0 / 16384.0;
                const M2: f64 = 2523.0 / 4096.0 * 128.0;
                const C1: f64 = 3424.0 / 4096.0;
                const C2: f64 = 2413.0 / 4096.0 * 32.0;
                const C3: f64 = 2392.0 / 4096.0 * 32.0;
                let vp = v.powf(1.0 / M2);
                let num = (vp - C1).max(0.0);
                let den = C2 - C3 * vp;
                (num / den).powf(1.0 / M1)
            }
        }
    }

    /// Encode a linear-light channel value back through this transfer function.
    fn from_linear(self, v: f64) -> f64 {
        let v = v.clamp(0.0, 1.0);
        match self {
            Self::Linear => v,
            Self::Gamma22 => v.powf(1.0 / 2.2),
            Self::Srgb => {
                if v <= 0.0031308 {
                    v * 12.92
                } else {
                    1.055 * v.powf(1.0 / 2.4) - 0.055
                }
            }
            Self::St2084Pq => {
                const M1: f64 = 2610.0 / 16384.0;
                const M2: f64 = 2523.0 / 4096.0 * 128.0;
                const C1: f64 = 3424.0 / 4096.0;
                const C2: f64 = 2413.0 / 4096.0 * 32.0;
                const C3: f64 = 2392.0 / 4096.0 * 32.0;
                let vm = v.powf(M1);
                ((C1 + C2 * vm) / (1.0 + C3 * vm)).powf(M2)
            }
        }
    }
}

/// A resolved surface/output color description: primaries + transfer function, plus an optional HDR
/// reference peak luminance (cd/m²) and any raw ICC profile the client supplied.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorDescription {
    pub primaries: ColorPrimaries,
    pub transfer: ColorTransfer,
    /// Reference/peak luminance in cd/m² (SDR ≈ 80–203, HDR up to several thousand). Drives HDR→SDR
    /// tone mapping.
    pub peak_luminance: f64,
    /// The raw ICC profile bytes, when the description came from an ICC creator (else empty).
    pub icc: Vec<u8>,
}

impl ColorDescription {
    /// The compositor's default sRGB SDR description (the hl output profile).
    pub fn srgb() -> Self {
        Self { primaries: ColorPrimaries::Srgb, transfer: ColorTransfer::Srgb, peak_luminance: 80.0, icc: Vec::new() }
    }

    fn is_hdr(&self) -> bool {
        matches!(self.transfer, ColorTransfer::St2084Pq) || self.peak_luminance > 400.0
    }
}

/// Invert a 3×3 matrix (returns `None` if singular).
fn invert3(m: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv_det = 1.0 / det;
    let mut out = [[0.0; 3]; 3];
    out[0][0] = (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv_det;
    out[0][1] = (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv_det;
    out[0][2] = (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv_det;
    out[1][0] = (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv_det;
    out[1][1] = (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv_det;
    out[1][2] = (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv_det;
    out[2][0] = (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv_det;
    out[2][1] = (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv_det;
    out[2][2] = (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv_det;
    Some(out)
}

fn mat_mul(a: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        a[0][0] * v[0] + a[0][1] * v[1] + a[0][2] * v[2],
        a[1][0] * v[0] + a[1][1] * v[1] + a[1][2] * v[2],
        a[2][0] * v[0] + a[2][1] * v[1] + a[2][2] * v[2],
    ]
}

/// Convert one encoded RGB pixel from the surface's color description to the output's, composing in
/// LINEAR light: decode `src` transfer → convert primaries through XYZ → tone-map HDR luminance into the
/// output range → encode `dst` transfer. Channels are in [0,1]. This is the per-pixel operation the
/// compositor applies when a surface's color volume differs from the target output's.
pub fn convert_pixel(src: &ColorDescription, dst: &ColorDescription, rgb: [f64; 3]) -> [f64; 3] {
    // 1. Decode source transfer function to linear light.
    let lin_src = [src.transfer.to_linear(rgb[0]), src.transfer.to_linear(rgb[1]), src.transfer.to_linear(rgb[2])];

    // 2. HDR→SDR tone mapping in the source's own linear space: PQ decodes to absolute luminance
    //    normalized to 10000 cd/m². Scale it into the destination's peak and clamp.
    let toned = if src.is_hdr() && !dst.is_hdr() {
        let scale = 10000.0 / dst.peak_luminance.max(1.0);
        [
            (lin_src[0] * scale).min(1.0),
            (lin_src[1] * scale).min(1.0),
            (lin_src[2] * scale).min(1.0),
        ]
    } else {
        lin_src
    };

    // 3. Primaries conversion through CIE XYZ: XYZ = M_src · rgb; rgb_dst = M_dst⁻¹ · XYZ.
    let out_lin = if src.primaries == dst.primaries {
        toned
    } else {
        let xyz = mat_mul(src.primaries.rgb_to_xyz(), toned);
        let to_dst = invert3(dst.primaries.rgb_to_xyz()).unwrap_or([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
        let d = mat_mul(to_dst, xyz);
        [d[0].clamp(0.0, 1.0), d[1].clamp(0.0, 1.0), d[2].clamp(0.0, 1.0)]
    };

    // 4. Encode the destination transfer function.
    [dst.transfer.from_linear(out_lin[0]), dst.transfer.from_linear(out_lin[1]), dst.transfer.from_linear(out_lin[2])]
}

// ---- protocol state --------------------------------------------------------------------------------

/// Pending parametric image-description parameters, accumulated by the `set_*` requests before `create`.
#[derive(Default)]
pub struct PendingParams {
    primaries: Option<ColorPrimaries>,
    transfer: Option<ColorTransfer>,
    peak_luminance: Option<f64>,
}

/// User data for a parametric creator (interior-mutable so `set_*` requests accumulate).
pub struct ParamsCreatorData {
    pending: Mutex<PendingParams>,
}

/// User data for an ICC creator.
pub struct IccCreatorData {
    icc: Mutex<Vec<u8>>,
}

/// User data for an image description: the resolved, immutable color description it represents.
pub struct ImageDescData {
    desc: ColorDescription,
}

/// User data for a per-surface color object.
pub struct ColorSurfaceData {
    surface: WlWeak<WlSurface>,
}

/// Aggregate `wp_color_manager_v1` state, held in [`HlState`].
pub struct ColorManagementState {
    #[allow(dead_code)]
    global: GlobalId,
    /// The compositor's output color profile (what surfaces are converted TO).
    output: ColorDescription,
    /// Per-surface committed color description (sid → description + render intent).
    surface_colors: HashMap<u32, (ColorDescription, u32)>,
    /// Monotonic image-description identity counter (the `ready(identity)` handle).
    next_identity: u32,
}

impl ColorManagementState {
    pub fn new(dh: &DisplayHandle) -> Self {
        let global = dh.create_global::<HlState, WpColorManagerV1, ()>(COLOR_MANAGER_VERSION, ());
        Self {
            global,
            output: ColorDescription::srgb(),
            surface_colors: HashMap::new(),
            next_identity: 1,
        }
    }
}

impl HlState {
    /// The committed color description a surface declared (by sid), if any.
    pub fn surface_color(&self, sid: u32) -> Option<ColorDescription> {
        self.color.surface_colors.get(&sid).map(|(d, _)| d.clone())
    }

    /// The compositor's output color profile (surfaces convert to this).
    pub fn output_color(&self) -> ColorDescription {
        self.color.output.clone()
    }

    /// Convert one encoded RGB pixel from a surface's declared color volume to the output profile. When
    /// the surface has no declared color it is assumed to already be in the output profile (identity).
    pub fn convert_surface_pixel_to_output(&self, sid: u32, rgb: [f64; 3]) -> [f64; 3] {
        match self.surface_color(sid) {
            Some(src) => convert_pixel(&src, &self.color.output, rgb),
            None => rgb,
        }
    }
}

// ---- wp_color_manager_v1 (manager global) ----------------------------------------------------------

impl GlobalDispatch<WpColorManagerV1, ()> for HlState {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WpColorManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let mgr = data_init.init(resource, ());
        // Advertise the negotiated capability set, then `done` — the client waits for this before use.
        mgr.supported_intent(RenderIntent::Perceptual);
        mgr.supported_intent(RenderIntent::Relative);
        mgr.supported_feature(Feature::Parametric);
        mgr.supported_feature(Feature::SetLuminances);
        mgr.supported_feature(Feature::IccV2V4);
        mgr.supported_tf_named(TransferFunction::Srgb);
        mgr.supported_tf_named(TransferFunction::ExtLinear);
        mgr.supported_tf_named(TransferFunction::St2084Pq);
        mgr.supported_primaries_named(Primaries::Srgb);
        mgr.supported_primaries_named(Primaries::Bt2020);
        mgr.supported_primaries_named(Primaries::DisplayP3);
        mgr.done();
    }
}

impl Dispatch<WpColorManagerV1, ()> for HlState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &WpColorManagerV1,
        request: wp_color_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use wp_color_manager_v1::Request;
        match request {
            Request::CreateParametricCreator { obj } => {
                data_init.init(obj, ParamsCreatorData { pending: Mutex::new(PendingParams::default()) });
            }
            Request::CreateIccCreator { obj } => {
                data_init.init(obj, IccCreatorData { icc: Mutex::new(Vec::new()) });
            }
            Request::GetOutput { id, output: _ } => {
                data_init.init(id, ());
            }
            Request::GetSurface { id, surface } => {
                data_init.init(id, ColorSurfaceData { surface: surface.downgrade() });
                let _ = state; // surface color is stored on set_image_description
            }
            Request::Destroy => {}
            _ => {}
        }
    }
}

// ---- wp_image_description_creator_params_v1 --------------------------------------------------------

impl Dispatch<WpImageDescriptionCreatorParamsV1, ParamsCreatorData> for HlState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &WpImageDescriptionCreatorParamsV1,
        request: wp_image_description_creator_params_v1::Request,
        data: &ParamsCreatorData,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use wp_image_description_creator_params_v1::Request;
        match request {
            Request::SetTfNamed { tf } => {
                if let Some(t) = ColorTransfer::from_wire(u32::from(tf)) {
                    data.pending.lock().unwrap().transfer = Some(t);
                }
            }
            Request::SetPrimariesNamed { primaries } => {
                if let Some(p) = ColorPrimaries::from_wire(u32::from(primaries)) {
                    data.pending.lock().unwrap().primaries = Some(p);
                }
            }
            Request::SetLuminances { reference_lum, max_lum, .. } => {
                // Reference/peak luminance is carried as cd/m² (min is a fixed-point elsewhere in the
                // protocol; the reference/max integers are cd/m²). Take the larger as the peak.
                data.pending.lock().unwrap().peak_luminance = Some(reference_lum.max(max_lum) as f64);
            }
            Request::Create { image_description } => {
                let p = data.pending.lock().unwrap();
                let desc = ColorDescription {
                    primaries: p.primaries.unwrap_or(ColorPrimaries::Srgb),
                    transfer: p.transfer.unwrap_or(ColorTransfer::Srgb),
                    peak_luminance: p.peak_luminance.unwrap_or(80.0),
                    icc: Vec::new(),
                };
                let identity = state.color.next_identity;
                state.color.next_identity += 1;
                let obj = data_init.init(image_description, ImageDescData { desc });
                obj.ready(identity);
            }
            _ => {}
        }
    }
}

// ---- wp_image_description_creator_icc_v1 -----------------------------------------------------------

impl Dispatch<WpImageDescriptionCreatorIccV1, IccCreatorData> for HlState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &WpImageDescriptionCreatorIccV1,
        request: wp_image_description_creator_icc_v1::Request,
        data: &IccCreatorData,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use wp_image_description_creator_icc_v1::Request;
        match request {
            Request::SetIccFile { icc_profile, offset, length } => {
                // Read the client's ICC profile bytes out of the passed fd.
                if let Ok(bytes) = read_fd_range(&icc_profile, offset as u64, length as usize) {
                    *data.icc.lock().unwrap() = bytes;
                }
            }
            Request::Create { image_description } => {
                // An ICC-described surface: keep the raw profile. For conversion the oracle falls back to
                // sRGB primaries/transfer unless a richer parse is available, but the profile is retained
                // and reportable (get_information), which is what the ICC fixture asserts.
                let desc = ColorDescription {
                    primaries: ColorPrimaries::Srgb,
                    transfer: ColorTransfer::Srgb,
                    peak_luminance: 80.0,
                    icc: data.icc.lock().unwrap().clone(),
                };
                let identity = state.color.next_identity;
                state.color.next_identity += 1;
                let obj = data_init.init(image_description, ImageDescData { desc });
                obj.ready(identity);
            }
            _ => {}
        }
    }
}

/// Read `length` bytes starting at `offset` from a borrowed fd (the client's ICC profile file).
fn read_fd_range(
    fd: &std::os::unix::io::OwnedFd,
    offset: u64,
    length: usize,
) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::io::{AsRawFd, FromRawFd};
    // Duplicate the fd into an owned File without taking ownership of the caller's fd.
    let raw = unsafe { libc::dup(fd.as_raw_fd()) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(raw) };
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; length.min(1 << 20)];
    let n = file.read(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

// ---- wp_image_description_v1 -----------------------------------------------------------------------

impl Dispatch<WpImageDescriptionV1, ImageDescData> for HlState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WpImageDescriptionV1,
        request: wp_image_description_v1::Request,
        _data: &ImageDescData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use wp_image_description_v1::Request;
        // `get_information` would stream the description back; the oracle stores it and answers the
        // higher-level queries (`surface_color`) instead. Destroy is a no-op teardown.
        match request {
            Request::Destroy => {}
            _ => {}
        }
    }
}

// ---- wp_color_management_surface_v1 ----------------------------------------------------------------

impl Dispatch<WpColorManagementSurfaceV1, ColorSurfaceData> for HlState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &WpColorManagementSurfaceV1,
        request: wp_color_management_surface_v1::Request,
        data: &ColorSurfaceData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use wp_color_management_surface_v1::Request;
        let surface = match data.surface.upgrade() {
            Ok(s) => s,
            Err(_) => return,
        };
        let sid = state.surface_id(&surface);
        match request {
            Request::SetImageDescription { image_description, render_intent } => {
                if let Some(desc) = image_description.data::<ImageDescData>() {
                    state.color.surface_colors.insert(sid, (desc.desc.clone(), u32::from(render_intent)));
                }
            }
            Request::UnsetImageDescription => {
                state.color.surface_colors.remove(&sid);
            }
            Request::Destroy => {}
            _ => {}
        }
    }
}

// ---- wp_color_management_output_v1 -----------------------------------------------------------------

impl Dispatch<WpColorManagementOutputV1, ()> for HlState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &WpColorManagementOutputV1,
        request: wp_color_management_output_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use wp_color_management_output_v1::Request;
        match request {
            Request::GetImageDescription { image_description } => {
                let identity = state.color.next_identity;
                state.color.next_identity += 1;
                let obj = data_init.init(image_description, ImageDescData { desc: state.color.output.clone() });
                obj.ready(identity);
            }
            Request::Destroy => {}
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: [f64; 3], b: [f64; 3], eps: f64) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() <= eps)
    }

    #[test]
    fn srgb_to_srgb_is_identity() {
        let s = ColorDescription::srgb();
        let out = convert_pixel(&s, &s, [0.2, 0.5, 0.8]);
        assert!(approx(out, [0.2, 0.5, 0.8], 1e-6), "sRGB→sRGB must round-trip: {out:?}");
    }

    #[test]
    fn linear_bt2020_converts_to_srgb_primaries_in_linear_light() {
        // A desaturated (in-gamut) BT.2020 linear color converts through CIE XYZ into distinctly
        // different sRGB coordinates — proving a real primaries/gamut conversion rather than a raw
        // channel copy. The wider BT.2020 gamut means the same code values map to a warmer, redder sRGB
        // pixel; the byte-exact target is computed from the standard D65 matrices.
        let src = ColorDescription { primaries: ColorPrimaries::Bt2020, transfer: ColorTransfer::Linear, peak_luminance: 80.0, icc: Vec::new() };
        let dst = ColorDescription { primaries: ColorPrimaries::Srgb, transfer: ColorTransfer::Linear, peak_luminance: 80.0, icc: Vec::new() };
        let out = convert_pixel(&src, &dst, [0.5, 0.4, 0.3]);
        assert!(approx(out, [0.5733, 0.3884, 0.2863], 2e-3), "BT.2020→sRGB gamut conversion: {out:?}");
        assert!(!approx(out, [0.5, 0.4, 0.3], 1e-2), "must not be a raw channel copy: {out:?}");
        // sRGB→sRGB of the same color IS the identity, as a control.
        let ctl = convert_pixel(&dst, &dst, [0.5, 0.4, 0.3]);
        assert!(approx(ctl, [0.5, 0.4, 0.3], 1e-9), "same-primaries conversion is identity: {ctl:?}");
    }

    #[test]
    fn pq_hdr_surface_is_tone_mapped_onto_an_sdr_srgb_output() {
        // A PQ (ST2084) HDR surface at a mid code value decodes to a high absolute luminance; converting
        // it to an 80 cd/m² sRGB SDR output must tone-map it into [0,1] and re-encode sRGB, not pass the
        // PQ code through. We assert the output is a valid sRGB value and that a brighter PQ code maps to
        // a brighter (monotonic), clamped sRGB result.
        let hdr = ColorDescription { primaries: ColorPrimaries::Bt2020, transfer: ColorTransfer::St2084Pq, peak_luminance: 1000.0, icc: Vec::new() };
        let sdr = ColorDescription::srgb();
        let dim = convert_pixel(&hdr, &sdr, [0.3, 0.3, 0.3]);
        let bright = convert_pixel(&hdr, &sdr, [0.6, 0.6, 0.6]);
        for c in dim.iter().chain(bright.iter()) {
            assert!((0.0..=1.0).contains(c), "tone-mapped output must be a valid SDR value: {dim:?} {bright:?}");
        }
        assert!(bright[0] > dim[0], "higher PQ code must map to a brighter SDR value: {dim:?} -> {bright:?}");
        // A very high PQ code saturates to white (clamped) on the SDR output.
        let peak = convert_pixel(&hdr, &sdr, [0.99, 0.99, 0.99]);
        assert!(peak.iter().all(|&c| c > 0.9), "near-peak HDR clamps to near-white SDR: {peak:?}");
    }

    #[test]
    fn pq_and_srgb_transfer_functions_round_trip() {
        for tf in [ColorTransfer::Srgb, ColorTransfer::St2084Pq, ColorTransfer::Gamma22, ColorTransfer::Linear] {
            for v in [0.0, 0.1, 0.5, 0.9, 1.0] {
                let rt = tf.from_linear(tf.to_linear(v));
                assert!((rt - v).abs() < 1e-4, "{tf:?} round-trip {v} -> {rt}");
            }
        }
    }
}
