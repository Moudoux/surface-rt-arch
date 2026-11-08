precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a;
uniform vec4 u_b;
uniform sampler2D u_tex;
void main() {
  float a = v_uv.x * v_uv.x; float b = v_uv.y * 0.5 + a; vec4 t = texture2D(u_tex, v_uv);
  gl_FragColor = vec4(a, b, t.r * b, t.g + a * 0.25);
}
