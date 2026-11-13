// Texture times per-batch colours: glyphs and shadow slices (coverage in every channel, u_mul = colour),
// icons and images (u_mul = opacity), the wallpaper, glass (u_mul = 1 - tint.a, u_add = tint). 1 EXEC.
precision mediump float;
uniform sampler2D u_tex;
uniform vec4 u_mul;
uniform vec4 u_add;
varying vec4 v_a;
void main() {
    gl_FragColor = texture2D(u_tex, v_a.xy) * u_mul + u_add;
}
