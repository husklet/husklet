#version 300 es
out mediump vec4 _usk_FragColor;
uniform mediump sampler2D _uuTextureSampler_0_S0;
in highp vec2 _uvlocalCoord_S0;
flat in highp vec4 _uvtexSubset_S0;
in highp float _uvcoverage_S0;
void main(){
  mediump vec4 _uoutputColor_S0 = vec4(1.0, 1.0, 1.0, 1.0);
  highp vec2 _utexCoord = _uvlocalCoord_S0;
  highp vec4 _usubset = _uvtexSubset_S0;
  (_utexCoord = clamp(_utexCoord, _usubset.xy, _usubset.zw));
  (_uoutputColor_S0 = texture(_uuTextureSampler_0_S0, _utexCoord, -0.5));
  highp float _ucoverage = _uvcoverage_S0;
  mediump vec4 _uoutputCoverage_S0 = vec4(_ucoverage);
  {
    (_usk_FragColor = (_uoutputColor_S0 * _uoutputCoverage_S0));
  }
}
