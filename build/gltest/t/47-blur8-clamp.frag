// Gaussian blur, 8 taps, diagonal direction, y clamped (Chromium/Skia fbef, build/gltest/chromium-blur): 8 taps, sigma 2.3.
//! filter linear
//! uniform _uudir_S1_c0 0.0109375 0.0109375
//! uniform _uuoffsetsAndKernel_S1_c0 -6.710278 0.004051 -4.779855 0.035541 -2.861904 0.150640 -0.952881 0.309768 0.952881 0.309768 2.861904 0.150640 4.779855 0.035541 6.710278 0.004051
//! uniform _uumatrix_S1_c0_c0 0.8 0 0  0 0.8 0  0.1 0.1 1
//! uniform _uuclamp_S1_c0_c0_c0 0.15 0.3 0.85 0.75
uniform mediump sampler2D _uuTextureSampler_0_S1;
uniform mediump vec2 _uudir_S1_c0;
uniform mediump vec4 _uuoffsetsAndKernel_S1_c0[14];
uniform highp vec4 _uuclamp_S1_c0_c0_c0;
uniform highp mat3 _uumatrix_S1_c0_c0;
uniform highp mat3 _uumatrix_S1;
varying highp vec2 v_uv;
void main(){
  highp vec2 _u_26_tmp_1_coords = v_uv;
  mediump vec4 _u_27_sum = vec4(0.0, 0.0, 0.0, 0.0);
  const highp int sbcf = 4;
  for (highp int _u_28_i = 0; (_u_28_i < sbcf); (++_u_28_i))
  {
    const highp float sbd0 = 0.0;
    const highp float sbd1 = 13.0;
    mediump vec4 _u_29_s = _uuoffsetsAndKernel_S1_c0[int(clamp(float(_u_28_i), sbd0, sbd1))];
    const highp float sbd2 = 1.0;
    highp vec2 _u_30_inCoord = (_uumatrix_S1_c0_c0 * vec3((_u_26_tmp_1_coords + (_u_29_s.x * _uudir_S1_c0)), sbd2)).xy;
    highp vec2 _u_31_subsetCoord = vec2(0.0, 0.0);
    (_u_31_subsetCoord.x = _u_30_inCoord.x);
    (_u_31_subsetCoord.y = _u_30_inCoord.y);
    highp vec2 _u_32_clampedCoord = vec2(0.0, 0.0);
    (_u_32_clampedCoord.x = _u_31_subsetCoord.x);
    (_u_32_clampedCoord.y = clamp(_u_31_subsetCoord.y, _uuclamp_S1_c0_c0_c0.y, _uuclamp_S1_c0_c0_c0.w));
    mediump vec4 _u_33_textureColor = texture2D(_uuTextureSampler_0_S1, _u_32_clampedCoord, -0.474999994);
    (_u_27_sum += (_u_29_s.y * _u_33_textureColor));
    const highp float sbd3 = 1.0;
    highp vec2 _u_34_inCoord = (_uumatrix_S1_c0_c0 * vec3((_u_26_tmp_1_coords + (_u_29_s.z * _uudir_S1_c0)), sbd3)).xy;
    highp vec2 _u_35_subsetCoord = vec2(0.0, 0.0);
    (_u_35_subsetCoord.x = _u_34_inCoord.x);
    (_u_35_subsetCoord.y = _u_34_inCoord.y);
    highp vec2 _u_36_clampedCoord = vec2(0.0, 0.0);
    (_u_36_clampedCoord.x = _u_35_subsetCoord.x);
    (_u_36_clampedCoord.y = clamp(_u_35_subsetCoord.y, _uuclamp_S1_c0_c0_c0.y, _uuclamp_S1_c0_c0_c0.w));
    mediump vec4 _u_37_textureColor = texture2D(_uuTextureSampler_0_S1, _u_36_clampedCoord, -0.474999994);
    (_u_27_sum += (_u_29_s.w * _u_37_textureColor));
  }
  mediump vec4 _uoutput_S1 = _u_27_sum;
  {
    (gl_FragColor = _uoutput_S1);
  }
}
