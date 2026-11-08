#version 120

struct buf
{
    mat4 matrix;
    float opacity;
    float size;
    float borderWidth;
    vec2 aspect;
    vec2 offset;
    vec4 radius;
    vec4 color;
    vec4 shadowColor;
    vec4 borderColor;
};

uniform buf ubuf;

varying vec2 uv;
attribute vec2 in_uv;
attribute vec4 in_vertex;
attribute float _qt_order;

void main()
{
    uv = (vec2(-1.0) + (in_uv * 2.0)) * ubuf.aspect;
    gl_Position = ubuf.matrix * in_vertex;
    gl_Position.z = _qt_order * gl_Position.w;
}

