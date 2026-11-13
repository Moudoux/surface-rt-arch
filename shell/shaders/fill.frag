// Plain colour (premultiplied, per vertex): insides of solid shapes, dim overlays. 1 EXEC.
precision mediump float;
varying vec4 v_a;
void main() {
    gl_FragColor = v_a;
}
