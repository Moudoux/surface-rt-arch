// gl_FragCoord.z (0.5 for z = 0) and .w (1 for w = 1).
precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
void main() {
    gl_FragColor = vec4(gl_FragCoord.z, gl_FragCoord.w * 0.5, v_uv.x, 1.0);
}
