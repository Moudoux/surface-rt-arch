// Gaussian blur, 16 taps, x clamped: packed offsets and weights (Chromium/Skia b110, build/gltest/chromium-blur): 16 taps, sigma 4.8.
//! filter linear
//! uniform _uudir_S1_c0 0.015625 0
//! uniform _uuoffsetsAndKernel_S1_c0 -14.842755 0.001321 -12.862568 0.004395 -10.882859 0.012319 -8.903567 0.029076 -6.924624 0.057794 -4.945958 0.096747 -2.967494 0.136397 -0.989151 0.161951 0.989151 0.161951 2.967494 0.136397 4.945958 0.096747 6.924624 0.057794 8.903567 0.029076 10.882859 0.012319 12.862568 0.004395 14.842755 0.001321
//! uniform _uumatrix_S1_c0_c0 0.8 0 0  0 0.8 0  0.1 0.1 1
//! uniform _uuclamp_S1_c0_c0_c0 0.3 0.15 0.75 0.85
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
  const highp int sbcf = 8;
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
    (_u_32_clampedCoord.x = clamp(_u_31_subsetCoord.x, _uuclamp_S1_c0_c0_c0.x, _uuclamp_S1_c0_c0_c0.z));
    (_u_32_clampedCoord.y = _u_31_subsetCoord.y);
    mediump vec4 _u_33_textureColor = texture2D(_uuTextureSampler_0_S1, _u_32_clampedCoord, -0.474999994);
    (_u_27_sum += (_u_29_s.y * _u_33_textureColor));
    const highp float sbd3 = 1.0;
    highp vec2 _u_34_inCoord = (_uumatrix_S1_c0_c0 * vec3((_u_26_tmp_1_coords + (_u_29_s.z * _uudir_S1_c0)), sbd3)).xy;
    highp vec2 _u_35_subsetCoord = vec2(0.0, 0.0);
    (_u_35_subsetCoord.x = _u_34_inCoord.x);
    (_u_35_subsetCoord.y = _u_34_inCoord.y);
    highp vec2 _u_36_clampedCoord = vec2(0.0, 0.0);
    (_u_36_clampedCoord.x = clamp(_u_35_subsetCoord.x, _uuclamp_S1_c0_c0_c0.x, _uuclamp_S1_c0_c0_c0.z));
    (_u_36_clampedCoord.y = _u_35_subsetCoord.y);
    mediump vec4 _u_37_textureColor = texture2D(_uuTextureSampler_0_S1, _u_36_clampedCoord, -0.474999994);
    (_u_27_sum += (_u_29_s.w * _u_37_textureColor));
  }
  mediump vec4 _uoutput_S1 = _u_27_sum;
  {
    (gl_FragColor = _uoutput_S1);
  }
}
