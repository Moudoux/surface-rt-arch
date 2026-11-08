precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a;
uniform vec4 u_b;
uniform sampler2D u_tex;
void main() {
  vec2 p = v_uv * 3.0; gl_FragColor = vec4(fract(p.x), floor(p.y) / 3.0, 0.0, 1.0);
}
