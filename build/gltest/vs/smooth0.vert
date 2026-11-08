attribute vec2 a_pos;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a, u_b;
void main() {
  v_uv = a_pos * 0.5 + 0.5;
  float k = u_a.w; /* 1.0, keeps the matrix a uniform */
  mat4 matrix = mat4(vec4(k * 2.0 / 320.0, 0.0, 0.0, 0.0), vec4(0.0, k * -2.0 / 240.0, 0.0, 0.0),
                     vec4(0.0, 0.0, k, 0.0), vec4(-k, k, 0.0, k));
  vec2 pixelSize = vec2(2.0 / 320.0, 2.0 / 240.0) * k;
  vec4 vertex = vec4(140.0 + 40.0 * a_pos.x, 100.0 + 40.0 * a_pos.y, 0.0, 1.0);
  vec4 vertexOffset = vec4(max(a_pos, 0.0) * 0.7, 0.0, 0.0);
  vec4 pos = matrix * vertex;
  vec4 m0 = matrix[0];
  vec4 m1 = matrix[1];
  vec4 P = pos;
  if (vertexOffset.x != 0.0) {
    vec4 delta = m0 * vertexOffset.x;
    vec2 dir = (delta.xy * pos.w) - (pos.xy * delta.w);
    vec2 ndir = (pixelSize * 0.5) * normalize(dir / pixelSize);
    dir -= ((ndir * delta.w) * pos.w);
    float numerator = dot(dir, (ndir * pos.w) * pos.w);
    float scale = 0.0;
    if (numerator < 0.0) { scale = 1.0; } else { scale = min(1.0, numerator / dot(dir, dir)); }
    P += (delta * scale);
  }
  if (vertexOffset.y != 0.0) {
    vec4 delta_1 = m1 * vertexOffset.y;
    vec2 dir_1 = (delta_1.xy * pos.w) - (pos.xy * delta_1.w);
    vec2 ndir_1 = (pixelSize * 0.5) * normalize(dir_1 / pixelSize);
    dir_1 -= ((ndir_1 * delta_1.w) * pos.w);
    float numerator_1 = dot(dir_1, (ndir_1 * pos.w) * pos.w);
    float scale_1 = 0.0;
    if (numerator_1 < 0.0) { scale_1 = 1.0; } else { scale_1 = min(1.0, numerator_1 / dot(dir_1, dir_1)); }
    P += (delta_1 * scale_1);
  }
  /* (P - pos) is the AA offset in clip space, tiny: scale up */
  v_col = vec4(P.xy * 0.5 + 0.5, (P.xy - pos.xy) * 40.0 + 0.5);
  gl_Position = vec4(a_pos, 0.0, 1.0);
}
