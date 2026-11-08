precision mediump float;
varying vec2 v_uv;
uniform vec4 u_a, u_b;
/* dependent chain with saturated, negated and absolute intermediates
 * (ALU result forwarding between packets) */
void main() {
  float x = clamp(v_uv.x * 3.0 - 1.0, 0.0, 1.0);
  float y = clamp(x * u_b.x - 0.5, 0.0, 1.0);
  float z = abs(y - 0.6) * 1.5;
  float w = -z * u_a.y + 0.75;
  float q = max(w, v_uv.y * 0.5);
  gl_FragColor = vec4(x, y, z, q);
}
