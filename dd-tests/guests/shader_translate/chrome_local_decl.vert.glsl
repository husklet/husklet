#version 300 es
uniform highp vec4 _usk_RTAdjust;
in highp vec2 _uposition;
in highp float _ucoverage;
in highp vec2 _ulocalCoord;
in highp vec4 _utexSubset;
out highp vec2 _uvlocalCoord_S0;
flat out highp vec4 _uvtexSubset_S0;
out highp float _uvcoverage_S0;
void main(){
  (gl_Position = vec4(0.0, 0.0, 0.0, 0.0));
  highp vec2 _uposition = _uposition;
  (_uvlocalCoord_S0 = _ulocalCoord);
  (_uvtexSubset_S0 = _utexSubset);
  (_uvcoverage_S0 = _ucoverage);
  const highp float s1603 = 0.0;
  const highp float s1604 = 1.0;
  (gl_Position = vec4(_uposition, s1603, s1604));
  const highp float s1605 = 0.0;
  (gl_Position = vec4(((gl_Position.xy * _usk_RTAdjust.xz) + (gl_Position.ww * _usk_RTAdjust.yw)), s1605, gl_Position.w));
}
