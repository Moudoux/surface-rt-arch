// Edge pieces of solid rounded rects (corners, 2 px bands): colour times anti-aliased coverage, written into
// alpha (no discard). Two varyings: only for thin pieces, interiors use fill.frag.
precision mediump float;
varying vec4 v_a;
varying vec4 v_b;
void main() {
    vec2 q = v_b.xy;
    float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - v_b.z;
    gl_FragColor = v_a * clamp(0.5 - d, 0.0, 1.0);
}
