precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a;
uniform vec4 u_b;
uniform sampler2D u_tex;
void main() {
  vec4 t = texture2D(u_tex, v_uv);
  if (t.a < 0.6) discard;
  gl_FragColor = t * u_a;
}
