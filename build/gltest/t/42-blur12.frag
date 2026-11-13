// Gaussian blur, 12 taps: 32 constants (Chromium/Skia cdd1, build/gltest/chromium-blur): 12 taps, sigma 3.5.
//! filter linear
//! uniform _uudir_S1_c0 0.015625 0
//! uniform _uuoffsetsAndKernel_S1_c0 -10.789470 0.001783 -8.824166 0.008842 -6.860907 0.031812 -4.899353 0.083084 -2.939080 0.157545 -0.979603 0.216934 0.979603 0.216934 2.939080 0.157545 4.899353 0.083084 6.860907 0.031812 8.824166 0.008842 10.789470 0.001783
//! uniform _uumatrix_S1_c0_c0 0.8 0 0  0 0.8 0  0.1 0.1 1
uniform mediump sampler2D _uuTextureSampler_0_S1;
uniform mediump vec2 _uudir_S1_c0;
uniform mediump vec4 _uuoffsetsAndKernel_S1_c0[14];
uniform highp mat3 _uumatrix_S1_c0_c0;
uniform highp mat3 _uumatrix_S1;
varying highp vec2 v_uv;
void main(){
  highp vec2 _u_6_tmp_1_coords = v_uv;
  mediump vec4 _u_7_sum = vec4(0.0, 0.0, 0.0, 0.0);
  const highp int sbc6 = 6;
  for (highp int _u_8_i = 0; (_u_8_i < sbc6); (++_u_8_i))
  {
    const highp float sbc7 = 0.0;
    const highp float sbc8 = 13.0;
    mediump vec4 _u_9_s = _uuoffsetsAndKernel_S1_c0[int(clamp(float(_u_8_i), sbc7, sbc8))];
    const highp float sbc9 = 1.0;
    (_u_7_sum += (_u_9_s.y * texture2D(_uuTextureSampler_0_S1, (_uumatrix_S1_c0_c0 * vec3((_u_6_tmp_1_coords + (_u_9_s.x * _uudir_S1_c0)), sbc9)).xy, -0.474999994)));
    const highp float sbca = 1.0;
    (_u_7_sum += (_u_9_s.w * texture2D(_uuTextureSampler_0_S1, (_uumatrix_S1_c0_c0 * vec3((_u_6_tmp_1_coords + (_u_9_s.z * _uudir_S1_c0)), sbca)).xy, -0.474999994)));
  }
  mediump vec4 _uoutput_S1 = _u_7_sum;
  {
    (gl_FragColor = _uoutput_S1);
  }
}
