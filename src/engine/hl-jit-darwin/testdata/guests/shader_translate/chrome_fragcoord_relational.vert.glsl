#version 300 es
uniform highp vec4 _usk_RTAdjust;
in highp vec2 _uposition;
in mediump vec4 _ucolor;
flat out mediump vec4 _uvcolor_S0;
void main(){
  (gl_Position = vec4(0.0, 0.0, 0.0, 0.0));
  (_uvcolor_S0 = _ucolor);
  const highp float s15fe = 0.0;
  const highp float s15ff = 1.0;
  (gl_Position = vec4(_uposition, s15fe, s15ff));
  const highp float s1600 = 0.0;
  (gl_Position = vec4(((gl_Position.xy * _usk_RTAdjust.xz) + (gl_Position.ww * _usk_RTAdjust.yw)), s1600, gl_Position.w));
}
