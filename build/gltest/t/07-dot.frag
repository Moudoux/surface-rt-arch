precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a;
uniform vec4 u_b;
uniform sampler2D u_tex;
void main() {
  float l = dot(v_col.rgb, vec3(0.299, 0.587, 0.114)); gl_FragColor = vec4(l, l, l, 1.0);
}
