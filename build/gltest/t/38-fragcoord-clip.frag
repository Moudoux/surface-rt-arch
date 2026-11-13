// Skia's non-AA inverse rect clip (Chromium inset box-shadow): colour varying times
// 1 - inside(rect), rect in window coordinates.
precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
uniform vec4 u_a;
void main() {
    highp vec4 rect = vec4(16.0, 12.0, 40.0, 50.0);
    float coverage = float(all(greaterThan(vec4(gl_FragCoord.xy, rect.zw), vec4(rect.xy, gl_FragCoord.xy))));
    gl_FragColor = v_col * (1.0 - coverage);
}
