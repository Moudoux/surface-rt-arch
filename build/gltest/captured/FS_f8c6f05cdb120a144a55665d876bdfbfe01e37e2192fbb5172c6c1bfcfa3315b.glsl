#version 120

struct qt_buf
{
    mat4 qt_Matrix;
    float qt_Opacity;
};

uniform qt_buf qt_ubuf;

uniform sampler2D source;

varying vec2 qt_TexCoord0;

void main()
{
    gl_FragData[0] = texture2D(source, qt_TexCoord0) * qt_ubuf.qt_Opacity;
}

