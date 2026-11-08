precision mediump float;
varying vec2 v_uv;
uniform vec4 u_a, u_b;
/* x * -2 + 3 (x2 source modifier instead of a constant), smoothstep, and
 * (uv * u + v) * w (reassociated into one MAD with preamble products) */
void main() {
  float s = v_uv.x * -2.0 + 3.0;
  float t = smoothstep(0.1, 0.9, v_uv.y);
  vec2 k = (v_uv * u_a.xy + u_a.zw) * u_b.w;
  float m = (v_uv.x + u_b.y) * -u_a.y;
  gl_FragColor = vec4(s * 0.25, t, k.x + k.y, m + 0.5);
}
