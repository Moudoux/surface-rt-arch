precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a;
uniform vec4 u_b;
uniform sampler2D u_tex;
void main() {
  gl_FragColor = vec4(min(v_uv.x, 0.5), max(v_uv.y, 0.5), 0.0, 1.0);
}
