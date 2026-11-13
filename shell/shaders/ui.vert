// rtshell UI vertex shader (GLSL ES 1.00). One vertex layout for every UI program, so a whole frame lives in
// one VBO: position in physical output pixels (u_proj = Smithay's frame projection: output transform + flip)
// and two generic vec4s whose meaning depends on the program (render/batch.rs):
//   fill:     a = premultiplied colour
//   tex:      a.xy = texture coordinate
//   rrect:    a = premultiplied colour, b = (qx, qy, r): distance past the inner box, corner radius
//   texedge:  a.xy = texture coordinate, b = (qx, qy, r)
// On grate every varying a fragment shader reads costs a lot (a second one ~2.5x, a third ~5x per pixel), so
// large areas use programs that read one varying, and per-batch constants are uniforms.
uniform mat3 u_proj;
attribute vec2 a_pos;
attribute vec4 a_a;
attribute vec4 a_b;
varying vec4 v_a;
varying vec4 v_b;
void main() {
    v_a = a_a;
    v_b = a_b;
    gl_Position = vec4((u_proj * vec3(a_pos, 1.0)).xy, 0.0, 1.0);
}
