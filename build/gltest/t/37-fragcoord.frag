// gl_FragCoord.xy (window position, 64x64 target) next to an ordinary varying.
precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
void main() {
    gl_FragColor = vec4(gl_FragCoord.xy / 64.0, v_col.z, 1.0);
}
