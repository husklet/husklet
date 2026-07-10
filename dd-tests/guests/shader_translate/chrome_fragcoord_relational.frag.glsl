#version 300 es
uniform highp vec2 _uu_skRTFlip;
out mediump vec4 _usk_FragColor;
uniform highp vec4 _uurectUniform_S1_c0;
flat in mediump vec4 _uvcolor_S0;
void main(){
  highp vec4 _usk_FragCoord = vec4(gl_FragCoord.x, (_uu_skRTFlip.x + (_uu_skRTFlip.y * gl_FragCoord.y)), gl_FragCoord.z, gl_FragCoord.w);
  mediump vec4 _uoutputColor_S0 = _uvcolor_S0;
  mediump float _u_3_coverage = float(all(greaterThan(vec4(_usk_FragCoord.xy, _uurectUniform_S1_c0.zw), vec4(_uurectUniform_S1_c0.xy, _usk_FragCoord.xy))));
  {
    const mediump float s1602 = 1.0;
    (_u_3_coverage = (s1602 - _u_3_coverage));
  }
  mediump vec4 _uoutput_S1 = vec4(_u_3_coverage);
  {
    (_usk_FragColor = (_uoutputColor_S0 * _uoutput_S1));
  }
}
