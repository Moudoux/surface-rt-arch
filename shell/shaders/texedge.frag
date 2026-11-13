// Edge pieces of textured rounded shapes (glass, images): tex.frag times coverage. Thin pieces only.
precision mediump float;
uniform sampler2D u_tex;
uniform vec4 u_mul;
uniform vec4 u_add;
varying vec4 v_a;
varying vec4 v_b;
void main() {
    vec2 q = v_b.xy;
    float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - v_b.z;
    gl_FragColor = (texture2D(u_tex, v_a.xy) * u_mul + u_add) * clamp(0.5 - d, 0.0, 1.0);
}
