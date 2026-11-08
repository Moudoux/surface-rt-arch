#version 120

struct buf
{
    mat4 qt_Matrix;
    float qt_Opacity;
    mat4 colorMatrix;
};

uniform buf _26;

uniform sampler2D source;

varying vec2 qt_TexCoord0;

void main()
{
    vec4 tex = texture2D(source, qt_TexCoord0);
    gl_FragData[0] = (tex * _26.colorMatrix) * _26.qt_Opacity;
}

