precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a;
uniform vec4 u_b;
uniform sampler2D u_tex;
void main() {
  /* KWin Modulate: texture * modulation, here (0.75, 0.25, 0.5, 0.5) */
  gl_FragColor = texture2D(u_tex, v_uv) * u_a.zyxx;
}
