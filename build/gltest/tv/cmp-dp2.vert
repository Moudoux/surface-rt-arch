attribute vec2 a_pos;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a;
void main() {
  v_uv = a_pos * 0.5 + 0.5;
  float d = dot(a_pos, u_a.xy);
  v_col = vec4(d > 0.0 ? 0.75 : 0.25, a_pos.y < 0.0 ? u_a.y : u_a.z,
               clamp(d * 0.5 + 0.5, 0.0, 1.0), 1.0);
  gl_Position = vec4(a_pos, 0.0, 1.0);
}
