use hl_gpu::{GpuError, Result};

const HELPERS: &str = r#"
fn _hl_cubic_address(i: i32, n: i32, mode: u32) -> i32 {
    if mode == 4u { return i; }
    if mode == 0u || mode == 3u { return clamp(i, 0, n - 1); }
    let p = ((i % n) + n) % n;
    if mode == 1u { return p; }
    let q = ((i % (2 * n)) + 2 * n) % (2 * n);
    return select(2 * n - 1 - q, q, q < n);
}
fn _hl_border_color(bc:u32)->vec4<f32>{
    if bc==4u || bc==5u { return vec4(1.0); }
    if bc==2u || bc==3u { return vec4(0.0,0.0,0.0,1.0); }
    return vec4(0.0);
}
fn _hl_mirror_clamp_uv(uv: vec2<f32>, au: u32, av: u32) -> vec2<f32> {
    return vec2(select(uv.x, abs(uv.x), au == 3u), select(uv.y, abs(uv.y), av == 3u));
}
fn _hl_cubic_weights(t: f32) -> vec4<f32> {
    let t2 = t * t; let t3 = t2 * t;
    return vec4(-0.5*t+t2-0.5*t3, 1.0-2.5*t2+1.5*t3, 0.5*t+2.0*t2-1.5*t3, -0.5*t2+0.5*t3);
}
fn _hl_cubic_level(t: texture_2d<f32>, uv: vec2<f32>, level: i32, au: u32, av: u32, bc:u32) -> vec4<f32> {
    let muv = _hl_mirror_clamp_uv(uv, au, av);
    let size = vec2<i32>(textureDimensions(t, level));
    let p = muv * vec2<f32>(size) - vec2(0.5);
    let base = vec2<i32>(floor(p)) - vec2(1);
    let w = mat2x4<f32>(_hl_cubic_weights(fract(p.x)), _hl_cubic_weights(fract(p.y)));
    var out = vec4(0.0);
    for (var y = 0; y < 4; y++) { for (var x = 0; x < 4; x++) {
        let q = vec2(_hl_cubic_address(base.x+x,size.x,au), _hl_cubic_address(base.y+y,size.y,av));
        let border=(au==4u && (q.x<0 || q.x>=size.x)) || (av==4u && (q.y<0 || q.y>=size.y));
        out += select(textureLoad(t, clamp(q,vec2(0),size-vec2(1)), level),_hl_border_color(bc),border) * w[0][x] * w[1][y];
    }}
    return out;
}
fn _hl_sample_auto(t: texture_2d<f32>, s: sampler, uv: vec2<f32>, mn:u32, mg:u32, mm:u32, au:u32, av:u32, aw:u32, bc:u32, lmin:u32, lmax:u32) -> vec4<f32> {
    let size = vec2<f32>(textureDimensions(t, 0));
    let rho = max(length(dpdx(uv*size)), length(dpdy(uv*size)));
    let lod = clamp(log2(max(rho, 0.000001)), bitcast<f32>(lmin), bitcast<f32>(lmax));
    let filter_mode = select(mg, mn, lod > 0.0);
    if filter_mode != 2u { return textureSample(t, s, _hl_mirror_clamp_uv(uv,au,av)); }
    let levels = i32(textureNumLevels(t));
    if mm == 1u {
        let lo = clamp(i32(floor(lod)), 0, levels-1); let hi = min(lo+1, levels-1);
        return mix(_hl_cubic_level(t,uv,lo,au,av,bc), _hl_cubic_level(t,uv,hi,au,av,bc), fract(lod));
    }
    return _hl_cubic_level(t, uv, clamp(i32(round(lod)),0,levels-1), au, av,bc);
}
fn _hl_sample_level(t: texture_2d<f32>, s: sampler, uv: vec2<f32>, lod0:f32, mn:u32, mg:u32, mm:u32, au:u32, av:u32, aw:u32, bc:u32, lmin:u32, lmax:u32) -> vec4<f32> {
    let lod = clamp(lod0, bitcast<f32>(lmin), bitcast<f32>(lmax));
    if select(mg,mn,lod > 0.0) != 2u { return textureSampleLevel(t,s,_hl_mirror_clamp_uv(uv,au,av),lod); }
    let levels=i32(textureNumLevels(t));
    if mm==1u { let lo=clamp(i32(floor(lod)),0,levels-1); let hi=min(lo+1,levels-1); return mix(_hl_cubic_level(t,uv,lo,au,av,bc),_hl_cubic_level(t,uv,hi,au,av,bc),fract(lod)); }
    return _hl_cubic_level(t,uv,clamp(i32(round(lod)),0,levels-1),au,av,bc);
}
fn _hl_sample_grad(t:texture_2d<f32>,s:sampler,uv:vec2<f32>,gx:vec2<f32>,gy:vec2<f32>,mn:u32,mg:u32,mm:u32,au:u32,av:u32,aw:u32,bc:u32,lmin:u32,lmax:u32)->vec4<f32>{
    let size=vec2<f32>(textureDimensions(t,0)); let lod=clamp(log2(max(max(length(gx*size),length(gy*size)),0.000001)),bitcast<f32>(lmin),bitcast<f32>(lmax));
    if select(mg,mn,lod>0.0)!=2u{
        let sign=vec2(select(1.0,-1.0,uv.x<0.0 && au==3u),select(1.0,-1.0,uv.y<0.0 && av==3u));
        return textureSampleGrad(t,s,_hl_mirror_clamp_uv(uv,au,av),gx*sign,gy*sign);
    }
    let levels=i32(textureNumLevels(t)); if mm==1u{let lo=clamp(i32(floor(lod)),0,levels-1);let hi=min(lo+1,levels-1);return mix(_hl_cubic_level(t,uv,lo,au,av,bc),_hl_cubic_level(t,uv,hi,au,av,bc),fract(lod));}
    return _hl_cubic_level(t,uv,clamp(i32(round(lod)),0,levels-1),au,av,bc);
}
"#;

pub(super) fn rewrite(mut source: String, layouts: &[crate::reflect::SamplerMetadataLayout]) -> Result<String> {
    if layouts.is_empty() { return Ok(source); }
    for (needle, helper, argc) in [("textureSampleGrad(","_hl_sample_grad",5usize),("textureSampleLevel(", "_hl_sample_level", 4usize), ("textureSample(", "_hl_sample_auto", 3usize)] {
        let mut at = 0;
        while let Some(relative) = source[at..].find(needle) {
            let start = at + relative;
            let open = start + needle.len() - 1;
            let (end, args) = arguments(&source, open)?;
            if args.len() < argc { return Err(GpuError::Invalid("wgpu: malformed texture sample")); }
            let Some((group, binding, index)) = sampler(&args[1]) else { at = end + 1; continue };
            let Some(layout) = layouts.iter().find(|layout| layout.group == group) else { at=end+1; continue };
            let Some(slot) = layout.samplers.iter().find(|slot| slot.binding == binding) else { at=end+1; continue };
            let ordinal = index.map_or(format!("{}u", slot.base_ordinal), |i| format!("({}u+u32({i}))", slot.base_ordinal));
            let meta = (0..9).map(|word| format!("_hl_sampler_metadata_g{group}_[{ordinal}*9u+{word}u]")).collect::<Vec<_>>().join(", ");
            let original = args[..argc].join(", ");
            source.replace_range(start..=end, &format!("{helper}({original}, {meta})"));
            at = start + helper.len();
        }
    }
    Ok(format!("{HELPERS}\n{source}"))
}

fn sampler(value: &str) -> Option<(u32,u32,Option<&str>)> {
    let value=value.trim(); let tail=value.strip_prefix("_hl_sampler_g")?;
    let (g,tail)=tail.split_once("_b")?; let digits=tail.find(|c:char| !c.is_ascii_digit()).unwrap_or(tail.len());
    let suffix=tail[digits..].strip_prefix('_').unwrap_or(&tail[digits..]);
    let index=if suffix.is_empty(){None}else{Some(suffix.strip_prefix('[')?.strip_suffix(']')?)};
    Some((g.parse().ok()?,tail[..digits].parse().ok()?,index))
}
fn arguments(source:&str, open:usize)->Result<(usize,Vec<String>)>{
    let mut depth=0i32; let mut last=open+1; let mut args=Vec::new();
    for (offset,ch) in source[open..].char_indices(){ match ch { '('|'['=>depth+=1, ')'|']'=>{depth-=1;if depth==0{args.push(source[last..open+offset].trim().into());return Ok((open+offset,args));}}, ',' if depth==1=>{args.push(source[last..open+offset].trim().into());last=open+offset+1;}, _=>{} } }
    Err(GpuError::Invalid("wgpu: unterminated texture sample"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reflect::{SamplerMetadataLayout,SamplerMetadataSlot};
    #[test]
    fn implicit_sample_rewrites_and_validates() {
        let source=r#"@group(0) @binding(0) var t:texture_2d<f32>;
@group(0) @binding(1) var _hl_sampler_g0_b1:sampler;
@group(0) @binding(2) var<storage,read> _hl_sampler_metadata_g0_:array<u32>;
@fragment fn main()->@location(0) vec4<f32>{return textureSample(t,_hl_sampler_g0_b1,vec2(0.5));}"#.into();
        let output=rewrite(source,&[SamplerMetadataLayout{group:0,binding:2,samplers:vec![SamplerMetadataSlot{binding:1,base_ordinal:0,count:1}]}]).unwrap();
        assert!(output.contains("_hl_sample_auto(t, _hl_sampler_g0_b1"));
        let module=naga::front::wgsl::parse_str(&output).unwrap();
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(),naga::valid::Capabilities::all()).validate(&module).unwrap();
    }
}
