#version 120

struct buf
{
    mat4 matrix;
    float opacity;
};

uniform buf _19;

attribute vec4 vertexCoord;
varying vec4 color;
attribute vec4 vertexColor;
attribute float _qt_order;

void main()
{
    gl_Position = _19.matrix * vertexCoord;
    color = vertexColor * _19.opacity;
    gl_Position.z = _qt_order * gl_Position.w;
}

