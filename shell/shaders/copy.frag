// One bilinear tap: halving a texture with it averages each 2x2 block exactly.
precision mediump float;
uniform sampler2D u_tex;
varying vec2 v_uv;
void main() {
    gl_FragColor = texture2D(u_tex, v_uv);
}
