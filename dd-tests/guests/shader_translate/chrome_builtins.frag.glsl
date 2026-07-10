// GLSL-ES builtins whose MSL spelling differs: mod (no MSL builtin), dFdx/dFdy (lowercase in MSL),
// inversesqrt (rsqrt), atan(y,x) (atan2). If any is mis-translated the shader fails to compile and the
// draw vanishes. This is the kind of gradient/pattern fragment Chrome emits for CSS gradients & AA.
precision mediump float;
varying vec2 vUv;
void main() {
    float stripe = mod(vUv.x * 10.0, 1.0);
    vec2 rep = mod(vUv * 8.0, 2.0);
    float aa = fwidth(stripe) + length(vec2(dFdx(stripe), dFdy(stripe)));
    float ang = atan(vUv.y - 0.5, vUv.x - 0.5);
    float inv = inversesqrt(dot(vUv, vUv) + 1.0);
    float t = clamp(stripe - aa, 0.0, 1.0);
    gl_FragColor = vec4(t, rep.x * 0.5, (ang / 6.2831853 + 0.5) * inv, 1.0);
}
