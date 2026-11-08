precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a;
uniform vec4 u_b;
uniform sampler2D u_tex;
void main() {
  float a = v_uv.x * v_uv.y; float b = v_uv.x + v_uv.y * 0.5; float c = v_uv.y * v_uv.y;
  float d = v_col.x * 0.75 + v_col.y; vec4 t = texture2D(u_tex, v_uv);
  gl_FragColor = vec4(a + t.r * 0.5, b * 0.5 + t.g * c, c + d * 0.25, d * 0.5 + t.b * a);
}
