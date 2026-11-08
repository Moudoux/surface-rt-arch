#version 120

struct buf
{
    mat4 qt_Matrix;
    float qt_Opacity;
    float shadowScale;
    vec2 shadowOffset;
    vec2 centerOffset;
    float contrast;
    float brightness;
    float saturation;
    vec4 colorizationColor;
    vec4 blurWeight1;
    vec2 blurWeight2;
    vec4 mask;
    float maskInverted;
    vec4 shadowColor;
    vec4 shadowBlurWeight1;
    vec2 shadowBlurWeight2;
};

uniform buf _25;

varying vec2 texCoord;
attribute vec2 qt_MultiTexCoord0;
attribute vec4 qt_Vertex;

void main()
{
    texCoord = qt_MultiTexCoord0;
    gl_Position = _25.qt_Matrix * qt_Vertex;
}

