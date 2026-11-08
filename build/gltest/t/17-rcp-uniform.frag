precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a;
uniform vec4 u_b;
uniform sampler2D u_tex;
void main() {
  gl_FragColor = vec4(1.0 / u_b.x, 1.0 / u_b.z, inversesqrt(u_b.z), 1.0);
}
