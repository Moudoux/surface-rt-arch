#version 120

struct qt_buf
{
    mat4 qt_Matrix;
    float qt_Opacity;
};

uniform qt_buf qt_ubuf;

varying vec2 qt_TexCoord0;
attribute vec2 qt_MultiTexCoord0;
attribute vec4 qt_Vertex;
attribute float _qt_order;

void main()
{
    qt_TexCoord0 = qt_MultiTexCoord0;
    gl_Position = qt_ubuf.qt_Matrix * qt_Vertex;
    gl_Position.z = _qt_order * gl_Position.w;
}

