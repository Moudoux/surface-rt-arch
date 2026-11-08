precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a;
uniform vec4 u_b;
uniform sampler2D u_tex;
void main() {
  float a = 1.0 / u_b.x; gl_FragColor = vec4(a, v_uv.x, 0.0, 1.0);
}
