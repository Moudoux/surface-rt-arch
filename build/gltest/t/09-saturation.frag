precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a;
uniform vec4 u_b;
uniform sampler2D u_tex;
void main() {
  vec4 c = texture2D(u_tex, v_uv); vec3 g = vec3(dot(c.rgb, vec3(0.2126, 0.7152, 0.0722))); gl_FragColor = vec4(mix(g, c.rgb, 0.5), c.a);
}
