precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a;
uniform vec4 u_b;
uniform sampler2D u_tex;
void main() {
  float a = v_uv.x * u_a.x; gl_FragColor = vec4(a, v_uv.y * u_a.y, 0.0, 1.0);
}
