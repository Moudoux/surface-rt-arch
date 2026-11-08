#version 120

uniform sampler2D qt_Texture;

varying vec2 qt_TexCoord;

void main()
{
    gl_FragData[0] = texture2D(qt_Texture, qt_TexCoord);
}

