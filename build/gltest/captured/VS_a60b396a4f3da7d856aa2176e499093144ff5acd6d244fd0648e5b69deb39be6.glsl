#version 120

struct buf
{
    mat4 qt_Matrix;
    float qt_Opacity;
    vec2 tilt;
};

uniform buf ubuf;

attribute vec4 aVertex;
varying vec2 vTexCoord;
attribute vec2 aTexCoord;
attribute float _qt_order;

void main()
{
    gl_Position = ubuf.qt_Matrix * aVertex;
    vTexCoord = aTexCoord;
    gl_Position.z = _qt_order * gl_Position.w;
}

