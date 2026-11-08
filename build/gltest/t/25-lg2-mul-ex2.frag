precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a;
uniform vec4 u_b;
uniform sampler2D u_tex;
void main() {
  float l = log2(v_uv.x + 0.5) * 2.0; gl_FragColor = vec4(exp2(l) * 0.25, v_uv.y, 0.0, 1.0);
}
