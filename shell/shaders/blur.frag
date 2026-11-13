// Separable Gaussian, one direction per pass. Unrolled (grate has no general loops); render/blur.rs replaces the
// placeholder line with one line per bilinear tap, pairing texels so N taps cover 2N-1 texels. u_step = one texel along
// the pass direction.
precision mediump float;
uniform sampler2D u_tex;
uniform vec2 u_step;
varying vec2 v_uv;
void main() {
    vec4 s = vec4(0.0);
@TAPS@
    gl_FragColor = s;
}
