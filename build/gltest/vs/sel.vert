attribute vec2 a_pos;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a, u_b;
void main() {
  v_uv = a_pos * 0.5 + 0.5;
  vec4 a = vec4(a_pos, -a_pos.x, a_pos.y * 0.0);
  vec4 r;
  r.x = a.x < 0.0 ? 0.2 : 0.8;
  r.y = a.y < 0.0 ? u_a.y : u_a.z;
  r.z = a.z < 0.0 ? 0.6 : u_b.w;
  r.w = a.w < 0.0 ? 0.1 : sqrt(a.y - 2.0) * 0.0 + 0.9;
  v_col = r;
  gl_Position = vec4(a_pos, 0.0, 1.0);
}
