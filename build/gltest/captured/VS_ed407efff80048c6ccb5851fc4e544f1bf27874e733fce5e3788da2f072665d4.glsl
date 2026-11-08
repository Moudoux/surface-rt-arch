#ifdef GL_ES
precision mediump float;
#endif
attribute vec4 v_position;
attribute vec4 v_texcoord;
varying vec2 source_texture;

void main()
{
    gl_Position = v_position;
    source_texture = v_texcoord.xy;
}
