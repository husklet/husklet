// The corpus data for `glsl_corpus.rs` (included via `include!`). Each `Case` is a real ANGLE/GskGpu-shaped
// GLSL-ES vertex+fragment pair exercising one construct family. `Path::Translate` entries are compiled for
// real through the executor's desktop naga route; `Path::Verbatim` entries assert the driver forwards them
// to the executor's ES route. Kept in a separate file so the harness (`glsl_corpus.rs`) stays legible.

fn corpus() -> Vec<Case> {
    vec![
        // ---------------------------------------------------------------------------------------------
        // control flow
        // ---------------------------------------------------------------------------------------------
        Case {
            name: "for_loop_constant_bound",
            vs: "#version 300 es\nin vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos,0.0,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nout vec4 o;\nvoid main(){ float s=0.0; for(int i=0;i<8;i++){ s+=0.1; } o=vec4(s); }\n",
            path: Path::Translate,
        },
        Case {
            name: "while_loop_dynamic_bound",
            vs: "#version 300 es\nin vec2 aPos;\nout float vN;\nuniform int uCount;\nvoid main(){ vN=float(uCount); gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nin float vN;\nout vec4 o;\nvoid main(){ float s=0.0; int i=0; while(i<int(vN)){ s+=0.05; i++; } o=vec4(s); }\n",
            path: Path::Translate,
        },
        Case {
            name: "if_else_chain",
            vs: "#version 300 es\nin vec2 aPos;\nout float vX;\nvoid main(){ vX=aPos.x; gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "#version 300 es\nprecision mediump float;\nin float vX;\nout vec4 o;\nvoid main(){ vec3 c; if(vX<0.0){c=vec3(1.0,0.0,0.0);} else if(vX<0.5){c=vec3(0.0,1.0,0.0);} else {c=vec3(0.0,0.0,1.0);} o=vec4(c,1.0); }\n",
            path: Path::Translate,
        },
        Case {
            name: "ternary_and_discard",
            vs: "attribute vec2 aPos;\nvarying vec2 vUV;\nvoid main(){ vUV=aPos; gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "precision mediump float;\nvarying vec2 vUV;\nvoid main(){ float a = vUV.x>0.5 ? 1.0 : 0.0; if(a<0.5) discard; gl_FragColor=vec4(a); }\n",
            path: Path::Translate,
        },
        Case {
            name: "switch_break_terminated",
            vs: "#version 300 es\nin vec2 aPos;\nflat out int vMode;\nuniform int uMode;\nvoid main(){ vMode=uMode; gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nflat in int vMode;\nout vec4 o;\nvoid main(){ vec3 c=vec3(0.0); switch(vMode){ case 0: c=vec3(1.0,0.0,0.0); break; case 1: c=vec3(0.0,1.0,0.0); break; default: c=vec3(0.0,0.0,1.0); break; } o=vec4(c,1.0); }\n",
            path: Path::Translate,
        },
        Case {
            name: "early_return_helper",
            vs: "#version 300 es\nin vec2 aPos;\nvoid main(){ gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nout vec4 o;\nfloat luma(vec3 c){ if(c.r>1.0) return 1.0; return dot(c, vec3(0.299,0.587,0.114)); }\nvoid main(){ o=vec4(vec3(luma(vec3(0.5))),1.0); }\n",
            path: Path::Translate,
        },
        Case {
            name: "do_while_loop",
            vs: "attribute vec2 aPos;\nvoid main(){ gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "precision highp float;\nvoid main(){ float s=0.0; int i=0; do { s+=0.1; i++; } while(i<5); gl_FragColor=vec4(s); }\n",
            path: Path::Translate,
        },
        Case {
            name: "nested_loops_break_continue",
            vs: "attribute vec2 aPos;\nvoid main(){ gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "precision highp float;\nvoid main(){ float s=0.0; for(int y=0;y<4;y++){ for(int x=0;x<4;x++){ if(x==y) continue; if(x+y>5) break; s+=0.05; } } gl_FragColor=vec4(s); }\n",
            path: Path::Translate,
        },
        Case {
            name: "nested_ternary_and_logical",
            vs: "attribute vec2 aPos;\nvarying float vX;\nvoid main(){ vX=aPos.x; gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "precision highp float;\nvarying float vX;\nvoid main(){ bool a=vX>0.0; bool b=vX<0.5; float r = (a&&b) ? 1.0 : (a||b ? 0.5 : 0.0); gl_FragColor=vec4(r); }\n",
            path: Path::Translate,
        },
        // ---------------------------------------------------------------------------------------------
        // data — arrays, structs, function params, multiple user functions
        // ---------------------------------------------------------------------------------------------
        Case {
            name: "local_array",
            vs: "attribute vec2 aPos;\nvoid main(){ gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "precision highp float;\nvoid main(){ float w[3]; w[0]=0.2; w[1]=0.3; w[2]=0.5; float s=w[0]+w[1]+w[2]; gl_FragColor=vec4(s); }\n",
            path: Path::Translate,
        },
        Case {
            name: "const_array_with_initializer",
            vs: "attribute vec2 aPos;\nvoid main(){ gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nout vec4 o;\nconst vec2 OFFS[4] = vec2[4](vec2(-1.0,-1.0), vec2(1.0,-1.0), vec2(-1.0,1.0), vec2(1.0,1.0));\nvoid main(){ vec2 a=OFFS[0]+OFFS[3]; o=vec4(a,0.0,1.0); }\n",
            path: Path::Translate,
        },
        Case {
            name: "global_struct_and_helper",
            vs: "#version 300 es\nin vec3 aPos;\nin vec3 aNormal;\nout vec3 vN;\nvoid main(){ vN=aNormal; gl_Position=vec4(aPos,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nin vec3 vN;\nout vec4 o;\nstruct Light { vec3 dir; vec3 color; };\nvec3 shade(Light l, vec3 n){ float d=max(dot(n, normalize(l.dir)), 0.0); return l.color*d; }\nvoid main(){ Light l; l.dir=vec3(0.0,1.0,0.0); l.color=vec3(1.0,0.9,0.8); o=vec4(shade(l, normalize(vN)),1.0); }\n",
            path: Path::Translate,
        },
        Case {
            name: "nested_structs",
            vs: "#version 300 es\nin vec3 aPos;\nvoid main(){ gl_Position=vec4(aPos,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nout vec4 o;\nstruct Material { vec3 albedo; float rough; };\nstruct Surface { Material mat; vec3 pos; };\nvoid main(){ Surface s; s.mat.albedo=vec3(0.8); s.mat.rough=0.5; s.pos=vec3(0.0); o=vec4(s.mat.albedo*s.mat.rough, 1.0); }\n",
            path: Path::Translate,
        },
        Case {
            name: "array_of_struct",
            vs: "#version 300 es\nin vec3 aPos;\nvoid main(){ gl_Position=vec4(aPos,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nout vec4 o;\nstruct Point { vec3 p; float w; };\nvoid main(){ Point pts[2]; pts[0].p=vec3(1.0,0.0,0.0); pts[0].w=0.5; pts[1].p=vec3(0.0,1.0,0.0); pts[1].w=0.5; vec3 c=pts[0].p*pts[0].w + pts[1].p*pts[1].w; o=vec4(c,1.0); }\n",
            path: Path::Translate,
        },
        Case {
            name: "out_inout_params_multi_fn",
            vs: "attribute vec2 aPos;\nvoid main(){ gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "precision highp float;\nvoid addOne(inout float x){ x += 1.0; }\nvoid split(float v, out float a, out float b){ a=v*0.25; b=v*0.75; }\nfloat combine(float a, float b){ return a+b; }\nvoid main(){ float v=1.0; addOne(v); float a; float b; split(v,a,b); gl_FragColor=vec4(combine(a,b)); }\n",
            path: Path::Translate,
        },
        Case {
            name: "helper_chain_calls_helper",
            vs: "attribute vec2 aPos;\nvoid main(){ gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "precision highp float;\nfloat sq(float x){ return x*x; }\nfloat len2(vec2 v){ return sq(v.x)+sq(v.y); }\nfloat falloff(vec2 v){ return 1.0/(1.0+len2(v)); }\nvoid main(){ gl_FragColor=vec4(falloff(vec2(0.5))); }\n",
            path: Path::Translate,
        },
        Case {
            name: "function_returns_struct",
            vs: "#version 300 es\nin vec3 aPos;\nvoid main(){ gl_Position=vec4(aPos,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nout vec4 o;\nstruct Ray { vec3 org; vec3 dir; };\nRay makeRay(vec3 p){ Ray r; r.org=p; r.dir=normalize(-p); return r; }\nvoid main(){ Ray r=makeRay(vec3(0.0,0.0,1.0)); o=vec4(r.dir*0.5+0.5, 1.0); }\n",
            path: Path::Translate,
        },
        Case {
            name: "global_mutable_var_and_dynamic_index",
            vs: "attribute vec2 aPos;\nvoid main(){ gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "precision highp float;\nuniform int uIdx;\nconst float K[5] = float[5](0.1, 0.2, 0.3, 0.4, 0.5);\nfloat gAccum;\nvoid main(){ gAccum = K[uIdx % 5]; gl_FragColor=vec4(gAccum); }\n",
            path: Path::Translate,
        },
        // ---------------------------------------------------------------------------------------------
        // types — matrices (square + non-square), swizzles, int/uint/bool + bitwise
        // ---------------------------------------------------------------------------------------------
        Case {
            // mat3/mat4 as std140 UBO members + mat2 exercised in the BODY (a 2-row matrix is unsupported
            // as a std140 block member — see the `mat2_in_std140_ubo` downstream-gap entry — but is fine as
            // a local/computed value, which is where the driver carries it).
            name: "mat234_square",
            vs: "#version 300 es\nin vec3 aPos;\nuniform mat3 uM3;\nuniform mat4 uM4;\nvoid main(){ mat2 r = mat2(1.0, 0.0, 0.0, 1.0); vec2 p = r * aPos.xy; vec3 q = uM3 * aPos; gl_Position = uM4 * vec4(p, q.z, 1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nout vec4 o;\nvoid main(){ o=vec4(1.0); }\n",
            path: Path::Translate,
        },
        Case {
            name: "nonsquare_matrices",
            vs: "#version 300 es\nin vec3 aPos;\nuniform mat3x4 uClip;\nuniform mat4x3 uBone;\nvoid main(){ vec3 skinned = uBone * vec4(aPos, 1.0); gl_Position = uClip * skinned; }\n",
            fs: "#version 300 es\nprecision highp float;\nout vec4 o;\nvoid main(){ o=vec4(1.0); }\n",
            path: Path::Translate,
        },
        Case {
            name: "swizzles_all_sets",
            vs: "attribute vec4 aPos;\nvarying vec4 vC;\nvoid main(){ vC = aPos.xyzw; vC.rgba = aPos.stpq; gl_Position = vec4(aPos.xy, aPos.zw); }\n",
            fs: "precision mediump float;\nvarying vec4 vC;\nvoid main(){ vec3 c = vC.bgr; float a = vC.a; gl_FragColor = vec4(c.xyz, a); }\n",
            path: Path::Translate,
        },
        Case {
            name: "matrix_construction_and_ops",
            vs: "#version 300 es\nin vec3 aPos;\nuniform mat4 uMVP;\nout vec3 vN;\nvoid main(){ mat3 basis = mat3(vec3(1.0,0.0,0.0), vec3(0.0,1.0,0.0), vec3(0.0,0.0,1.0)); mat3 nm = transpose(basis); vN = nm * aPos; gl_Position = uMVP * vec4(aPos, 1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nin vec3 vN;\nout vec4 o;\nvoid main(){ o = vec4(normalize(vN), 1.0); }\n",
            path: Path::Translate,
        },
        Case {
            name: "int_float_conversions_and_minmax",
            vs: "#version 300 es\nin vec2 aPos;\nflat out int vI;\nvoid main(){ vI = int(floor(aPos.x * 10.0)); gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nflat in int vI;\nout vec4 o;\nvoid main(){ float f = float(vI); float m = min(max(f, 0.0), 5.0); int back = int(m); o = vec4(float(back) / 5.0); }\n",
            path: Path::Translate,
        },
        Case {
            name: "int_uint_bool_bitwise_es3",
            vs: "#version 300 es\nin vec2 aPos;\nflat out uint vBits;\nuniform uint uSeed;\nvoid main(){ uint x = uSeed ^ 0x5cu; x = (x << 3u) | (x >> 2u); x &= 0xffu; vBits = x; gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nflat in uint vBits;\nout vec4 o;\nvoid main(){ bool hi = (vBits & 0x80u) != 0u; int s = hi ? -1 : 1; o = vec4(float(s) * float(vBits) / 255.0); }\n",
            path: Path::Translate,
        },
        // ---------------------------------------------------------------------------------------------
        // ES3 syntax — in/out, explicit layout(location), std140 UBO, flat, MRT
        // ---------------------------------------------------------------------------------------------
        Case {
            name: "es3_explicit_locations",
            vs: "#version 300 es\nlayout(location=0) in vec3 aPos;\nlayout(location=1) in vec2 aUV;\nout vec2 vUV;\nvoid main(){ vUV=aUV; gl_Position=vec4(aPos,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nin vec2 vUV;\nout vec4 o;\nvoid main(){ o=vec4(vUV,0.0,1.0); }\n",
            path: Path::Translate,
        },
        Case {
            name: "std140_ubo_block",
            vs: "#version 300 es\nin vec3 aPos;\nlayout(std140, binding=0) uniform Globals { mat4 uMVP; vec4 uTint; float uTime; };\nout vec4 vT;\nvoid main(){ vT=uTint*uTime; gl_Position=uMVP*vec4(aPos,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nin vec4 vT;\nout vec4 o;\nvoid main(){ o=vT; }\n",
            path: Path::Translate,
        },
        Case {
            name: "mrt_two_outputs",
            vs: "#version 300 es\nin vec3 aPos;\nout vec3 vN;\nvoid main(){ vN=aPos; gl_Position=vec4(aPos,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nin vec3 vN;\nlayout(location=0) out vec4 gColor;\nlayout(location=1) out vec4 gNormal;\nvoid main(){ gColor=vec4(1.0); gNormal=vec4(normalize(vN),1.0); }\n",
            path: Path::Translate,
        },
        // ---------------------------------------------------------------------------------------------
        // samplers — 2D, Cube, 2DArray, texture()/textureLod()/textureProj()/texelFetch()/textureSize()
        // ---------------------------------------------------------------------------------------------
        Case {
            name: "sampler2d_texture_and_lod",
            vs: "#version 300 es\nin vec2 aPos;\nout vec2 vUV;\nvoid main(){ vUV=aPos*0.5+0.5; gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nin vec2 vUV;\nuniform sampler2D uTex;\nout vec4 o;\nvoid main(){ o = texture(uTex, vUV) + textureLod(uTex, vUV, 2.0); }\n",
            path: Path::Translate,
        },
        Case {
            name: "sampler_cube",
            vs: "#version 300 es\nin vec3 aPos;\nout vec3 vDir;\nvoid main(){ vDir=aPos; gl_Position=vec4(aPos,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nin vec3 vDir;\nuniform samplerCube uEnv;\nout vec4 o;\nvoid main(){ o = texture(uEnv, normalize(vDir)); }\n",
            path: Path::Translate,
        },
        Case {
            name: "sampler2darray_and_size",
            vs: "#version 300 es\nin vec2 aPos;\nout vec2 vUV;\nvoid main(){ vUV=aPos*0.5+0.5; gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nin vec2 vUV;\nuniform sampler2DArray uArr;\nout vec4 o;\nvoid main(){ ivec3 sz = textureSize(uArr, 0); o = texture(uArr, vec3(vUV, 1.0)) * float(sz.z); }\n",
            path: Path::Translate,
        },
        Case {
            name: "texelfetch_and_proj",
            vs: "#version 300 es\nin vec2 aPos;\nout vec4 vProj;\nvoid main(){ vProj=vec4(aPos,0.0,1.0); gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nin vec4 vProj;\nuniform sampler2D uTex;\nout vec4 o;\nvoid main(){ vec4 a = texelFetch(uTex, ivec2(gl_FragCoord.xy), 0); vec4 b = textureProj(uTex, vProj); o = a + b; }\n",
            path: Path::Translate,
        },
        Case {
            name: "texture_offset_and_grad",
            vs: "#version 300 es\nin vec2 aPos;\nout vec2 vUV;\nvoid main(){ vUV=aPos*0.5+0.5; gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nin vec2 vUV;\nuniform sampler2D uTex;\nout vec4 o;\nvoid main(){ vec4 a = textureOffset(uTex, vUV, ivec2(1,0)); vec4 b = textureGrad(uTex, vUV, dFdx(vUV), dFdy(vUV)); o = a + b; }\n",
            path: Path::Translate,
        },
        Case {
            name: "separable_blur_loop_sampler",
            vs: "#version 300 es\nin vec2 aPos;\nout vec2 vUV;\nvoid main(){ vUV=aPos*0.5+0.5; gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nin vec2 vUV;\nuniform sampler2D uTex;\nuniform vec2 uTexel;\nout vec4 o;\nconst float W[3] = float[3](0.25, 0.5, 0.25);\nvoid main(){ vec4 sum = vec4(0.0); for(int i=-1;i<=1;i++){ sum += texture(uTex, vUV + vec2(float(i)*uTexel.x, 0.0)) * W[i+1]; } o = sum; }\n",
            path: Path::Translate,
        },
        Case {
            name: "multi_sampler_mixed",
            vs: "attribute vec2 aPos;\nvarying vec2 vUV;\nvoid main(){ vUV=aPos; gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "precision mediump float;\nvarying vec2 vUV;\nuniform sampler2D uAlbedo;\nuniform sampler2D uNormal;\nuniform samplerCube uEnv;\nvoid main(){ vec4 a=texture2D(uAlbedo,vUV); vec4 n=texture2D(uNormal,vUV); vec4 e=textureCube(uEnv,n.xyz); gl_FragColor=a+e; }\n",
            path: Path::Translate,
        },
        // ---------------------------------------------------------------------------------------------
        // builtins + gl_* variables
        // ---------------------------------------------------------------------------------------------
        Case {
            name: "math_builtins_bonanza",
            vs: "attribute vec3 aPos;\nvoid main(){ gl_Position=vec4(aPos,1.0); }\n",
            fs: "precision highp float;\nvoid main(){\n  vec3 a=vec3(0.3,0.6,0.9); vec3 b=vec3(1.0,0.5,0.25);\n  vec3 c = mix(a,b,0.5);\n  c = clamp(c, 0.0, 1.0);\n  float m = mod(c.x, 0.7) + fract(c.y);\n  float d = dot(a,b); vec3 x = cross(a,b);\n  float l = length(a); vec3 nn = normalize(b);\n  float p = pow(c.x, 2.0) + exp(c.y) + log(c.z + 1.0);\n  float st = step(0.5, c.x) + smoothstep(0.0, 1.0, c.y);\n  float sg = sign(c.z - 0.5) + floor(c.x) + ceil(c.y) + abs(c.z - 0.5);\n  gl_FragColor = vec4(c * (m + d + x.x + l + nn.x + p + st + sg), 1.0);\n}\n",
            path: Path::Translate,
        },
        Case {
            name: "gl_fragcoord",
            vs: "#version 300 es\nin vec2 aPos;\nvoid main(){ gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nout vec4 o;\nvoid main(){ o = vec4(gl_FragCoord.xy / 64.0, 0.0, 1.0); }\n",
            path: Path::Translate,
        },
        Case {
            name: "gl_frontfacing_branch",
            vs: "#version 300 es\nin vec3 aPos;\nout vec3 vN;\nvoid main(){ vN=aPos; gl_Position=vec4(aPos,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nin vec3 vN;\nout vec4 o;\nvoid main(){ vec3 n = gl_FrontFacing ? normalize(vN) : -normalize(vN); o = vec4(n*0.5+0.5, 1.0); }\n",
            path: Path::Translate,
        },
        // ---------------------------------------------------------------------------------------------
        // realistic composite ANGLE-shaped shaders (multiple families at once)
        // ---------------------------------------------------------------------------------------------
        Case {
            name: "composite_phong_lighting",
            vs: "#version 300 es\nlayout(location=0) in vec3 aPos;\nlayout(location=1) in vec3 aNormal;\nlayout(location=2) in vec2 aUV;\nlayout(std140, binding=0) uniform Camera { mat4 uViewProj; vec4 uEye; };\nout vec3 vWorld;\nout vec3 vNormal;\nout vec2 vUV;\nvoid main(){ vWorld=aPos; vNormal=aNormal; vUV=aUV; gl_Position=uViewProj*vec4(aPos,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nin vec3 vWorld;\nin vec3 vNormal;\nin vec2 vUV;\nuniform sampler2D uAlbedo;\nuniform vec4 uEye;\nout vec4 o;\nstruct Light { vec3 pos; vec3 color; };\nfloat diffuse(vec3 n, vec3 l){ return max(dot(n, l), 0.0); }\nfloat specular(vec3 n, vec3 l, vec3 v, float p){ vec3 h=normalize(l+v); return pow(max(dot(n,h),0.0), p); }\nvec3 shade(Light lt, vec3 n, vec3 world, vec3 albedo){ vec3 l=normalize(lt.pos-world); vec3 v=normalize(uEye.xyz-world); return albedo*lt.color*diffuse(n,l) + lt.color*specular(n,l,v,32.0); }\nvoid main(){ vec3 albedo=texture(uAlbedo, vUV).rgb; vec3 n=normalize(vNormal); Light lights[2]; lights[0].pos=vec3(5.0,5.0,5.0); lights[0].color=vec3(1.0,0.9,0.8); lights[1].pos=vec3(-5.0,2.0,1.0); lights[1].color=vec3(0.2,0.3,0.5); vec3 c=vec3(0.0); for(int i=0;i<2;i++){ c += shade(lights[i], n, vWorld, albedo); } o=vec4(c, 1.0); }\n",
            path: Path::Translate,
        },
        Case {
            name: "composite_gbuffer_mrt",
            vs: "#version 300 es\nlayout(location=0) in vec3 aPos;\nlayout(location=1) in vec3 aNormal;\nuniform mat4 uMVP;\nout vec3 vN;\nflat out int vMatId;\nuniform int uMatId;\nvoid main(){ vN=aNormal; vMatId=uMatId; gl_Position=uMVP*vec4(aPos,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nin vec3 vN;\nflat in int vMatId;\nlayout(location=0) out vec4 gAlbedo;\nlayout(location=1) out vec4 gNormal;\nvec3 palette(int id){ if(id==0) return vec3(1.0,0.0,0.0); else if(id==1) return vec3(0.0,1.0,0.0); return vec3(0.0,0.0,1.0); }\nvoid main(){ gAlbedo=vec4(palette(vMatId), 1.0); gNormal=vec4(normalize(vN)*0.5+0.5, 1.0); }\n",
            path: Path::Translate,
        },
        // ---------------------------------------------------------------------------------------------
        // precision qualifiers — statements + inline highp/mediump/lowp
        // ---------------------------------------------------------------------------------------------
        Case {
            name: "precision_statements_and_inline",
            vs: "#version 300 es\nprecision highp float;\nprecision highp int;\nin vec2 aPos;\nvoid main(){ highp float z = aPos.x; gl_Position=vec4(aPos, z*0.0, 1.0); }\n",
            fs: "#version 300 es\nprecision mediump float;\nout vec4 o;\nvoid main(){ lowp vec3 c = vec3(0.5); mediump float a = 1.0; o=vec4(c, a); }\n",
            path: Path::Translate,
        },
        // ---------------------------------------------------------------------------------------------
        // classic ES2 conformance shapes (the existing translate_render happy path — must stay green)
        // ---------------------------------------------------------------------------------------------
        Case {
            name: "es2_textured_mvp",
            vs: "attribute vec3 aPos;\nattribute vec2 aUV;\nvarying vec2 vUV;\nuniform mat4 uMVP;\nvoid main(){ vUV=aUV; gl_Position=uMVP*vec4(aPos,1.0); }\n",
            fs: "precision mediump float;\nvarying vec2 vUV;\nuniform sampler2D uTex;\nuniform vec4 uTint;\nvoid main(){ gl_FragColor = texture2D(uTex, vUV) * uTint; }\n",
            path: Path::Translate,
        },
        // ---------------------------------------------------------------------------------------------
        // VERBATIM route — vertex-pulling + combined-sampler helper parameter (the executor's ES route)
        // ---------------------------------------------------------------------------------------------
        Case {
            name: "gl_vertexid_pulling",
            vs: "#version 300 es\nout vec2 vUV;\nvoid main(){ vec2 p = vec2(float((gl_VertexID<<1)&2), float(gl_VertexID&2)); vUV=p; gl_Position=vec4(p*2.0-1.0,0.0,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nin vec2 vUV;\nout vec4 o;\nvoid main(){ o=vec4(vUV,0.0,1.0); }\n",
            path: Path::Verbatim,
        },
        Case {
            name: "gl_instanceid",
            vs: "#version 300 es\nin vec2 aPos;\nflat out int vId;\nvoid main(){ vId=gl_InstanceID; gl_Position=vec4(aPos + float(gl_InstanceID)*0.1, 0.0, 1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nflat in int vId;\nout vec4 o;\nvoid main(){ o=vec4(float(vId)*0.1); }\n",
            path: Path::Verbatim,
        },
        Case {
            name: "combined_sampler_helper_param",
            vs: "#version 300 es\nin vec2 aPos;\nout vec2 vUV;\nvoid main(){ vUV=aPos*0.5+0.5; gl_Position=vec4(aPos,0.0,1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nin vec2 vUV;\nuniform sampler2D uTex;\nout vec4 o;\nvec4 fetch(sampler2D t, vec2 p){ return texture(t, p); }\nvoid main(){ o = fetch(uTex, vUV); }\n",
            path: Path::Verbatim,
        },
        // ---------------------------------------------------------------------------------------------
        // KNOWN EXECUTOR-SIDE (naga) GAPS — the driver translates/reflects these correctly, but the
        // executor's naga step cannot compile them and no GL-side transform can paper over it. Documented
        // here (with the exact naga error) so the follow-up lands in the executor, not the driver.
        // ---------------------------------------------------------------------------------------------
        Case {
            // A 2-row matrix (mat2 / mat3x2 / mat4x2) as a std140 UNIFORM-BLOCK member. naga-24's `glsl-in`
            // rejects it in `front/glsl/offset.rs` (`rows == VectorSize::Bi` under Std140). mat3/mat4 and
            // the non-square mat3x4/mat4x3 (3- and 4-row) are all accepted, so this is narrow. A driver-side
            // fix would mean expanding the mat2 block member into 2×vec4 columns AND matching that in the
            // frame builder's std140 upload — that belongs in the executor (`hl_wip-gpu-wgpu`), which owns
            // both the naga compile and the layout, not the guest driver.
            name: "mat2_in_std140_ubo",
            vs: "#version 300 es\nin vec2 aPos;\nuniform mat2 uRot;\nvoid main(){ gl_Position = vec4(uRot * aPos, 0.0, 1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nout vec4 o;\nvoid main(){ o=vec4(1.0); }\n",
            path: Path::KnownDownstreamGap("UnsupportedMatrixTypeInStd140"),
        },
        Case {
            // `gl_PointSize`. WebGPU/WGSL has no point-size output (points are always 1px), so naga's
            // `wgsl-out` errors `Unsupported builtin PointSize` (`back/wgsl/writer.rs`). Nothing the GL
            // driver emits can change that — WGSL simply has no target for it. ANGLE/Chrome uses points
            // rarely; documented as an executor-side limitation.
            name: "gl_pointsize",
            vs: "#version 300 es\nin vec2 aPos;\nvoid main(){ gl_PointSize = 4.0; gl_Position = vec4(aPos, 0.0, 1.0); }\n",
            fs: "#version 300 es\nprecision highp float;\nout vec4 o;\nvoid main(){ o=vec4(1.0); }\n",
            path: Path::KnownDownstreamGap("Unsupported builtin PointSize"),
        },
    ]
}
