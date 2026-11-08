precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a;
uniform vec4 u_b;
uniform sampler2D u_tex;
void main() {
  if (v_uv.x < 0.4 || v_uv.y > 0.7) discard;
  gl_FragColor = vec4(v_uv, 0.25, 1.0);
}
