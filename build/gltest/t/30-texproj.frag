precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_tex;
void main() {
  float q = 1.0 + v_uv.x;
  gl_FragColor = texture2DProj(u_tex, vec3(v_uv * q, q));
}
