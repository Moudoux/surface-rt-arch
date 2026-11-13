// Skia's AA rect clip: coverage ramps over one pixel at the rect edges (needs sub-pixel gl_FragCoord).
precision mediump float;
varying vec2 v_uv;
varying vec4 v_col;
void main() {
    highp vec4 rect = vec4(8.25, 8.25, 56.5, 44.0);
    vec4 d = clamp(vec4(1.0, 1.0, -1.0, -1.0) * (gl_FragCoord.xyxy - rect), 0.0, 1.0);
    vec2 d2 = d.xy + d.zw - 1.0;
    gl_FragColor = v_col * (d2.x * d2.y);
}
