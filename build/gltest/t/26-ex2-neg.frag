precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a;
uniform vec4 u_b;
uniform sampler2D u_tex;
void main() {
  gl_FragColor = vec4(exp2(-v_uv.x * 2.0), exp2(v_uv.y * 2.0 - 1.0) * 0.5, 0.0, 1.0);
}
