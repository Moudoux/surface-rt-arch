#version 120

struct buf
{
    mat4 qt_Matrix;
    float qt_Opacity;
    float spread;
    vec2 dirstep;
    vec4 color;
    float thickness;
};

uniform buf _16;

uniform sampler2D source;

varying vec2 tL3;
varying vec2 tL2;
varying vec2 tL1;
varying vec2 tL0;
varying vec2 tR0;
varying vec2 tR1;
varying vec2 tR2;
varying vec2 tR3;

void main()
{
    gl_FragData[0] = mix(vec4(0.0), _16.color, vec4(clamp(((((((((0.0096828304231166839599609375 * texture2D(source, tL3).w) + (0.0605265013873577117919921875 * texture2D(source, tL2).w)) + (0.19388599693775177001953125 * texture2D(source, tL1).w)) + (0.3191539943218231201171875 * texture2D(source, tL0).w)) + (0.15265500545501708984375 * texture2D(source, tR0).w)) + (0.19388599693775177001953125 * texture2D(source, tR1).w)) + (0.0605265013873577117919921875 * texture2D(source, tR2).w)) + (0.0096828304231166839599609375 * texture2D(source, tR3).w)) / _16.thickness, 0.0, 1.0))) * _16.qt_Opacity;
}

