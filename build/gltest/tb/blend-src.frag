precision mediump float;
varying vec2 v_uv;
void main()
{
   /* distinct value per channel, alpha in (0.2, 0.9) */
   gl_FragColor = vec4(v_uv.x, v_uv.y, 0.5 * v_uv.x + 0.25, 0.2 + 0.7 * v_uv.y);
}
