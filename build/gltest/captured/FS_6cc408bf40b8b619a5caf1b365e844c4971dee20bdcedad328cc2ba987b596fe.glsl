#version 120

struct buf
{
    mat4 qt_Matrix;
    float qt_Opacity;
    vec2 tilt;
};

uniform buf ubuf;

uniform sampler2D uSource1;
uniform sampler2D uSource2;

varying vec2 vTexCoord;

void main()
{
    float f = dot(ubuf.tilt, vTexCoord - vec2(0.5));
    float df = length(vec2(dFdx(f), dFdy(f)));
    float blending = clamp(0.5 + (f / df), 0.0, 1.0);
    vec4 p1 = texture2D(uSource1, vTexCoord);
    vec4 p2 = texture2D(uSource2, vTexCoord);
    gl_FragData[0] = mix(p1, p2, vec4(blending)) * ubuf.qt_Opacity;
}

