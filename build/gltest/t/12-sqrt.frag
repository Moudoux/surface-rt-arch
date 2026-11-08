precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a;
uniform vec4 u_b;
uniform sampler2D u_tex;
void main() {
  gl_FragColor = vec4(sqrt(v_uv.x), inversesqrt(v_uv.y + 1.0), 0.0, 1.0);
}
