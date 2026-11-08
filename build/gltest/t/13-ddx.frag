#extension GL_OES_standard_derivatives : enable
precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a;
uniform vec4 u_b;
uniform sampler2D u_tex;
void main() {
  gl_FragColor = vec4(v_uv, 0.0, 1.0) + vec4(abs(dFdx(v_uv.x)), abs(dFdy(v_uv.y)), 0.0, 0.0);
}
