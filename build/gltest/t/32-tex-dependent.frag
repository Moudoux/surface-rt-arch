precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_tex;
void main() { gl_FragColor = texture2D(u_tex, texture2D(u_tex, v_uv).yx + 0.1); }
