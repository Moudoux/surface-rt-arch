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

uniform buf _23;

uniform sampler2D src;
uniform sampler2D blurSrc1;
uniform sampler2D blurSrc2;
uniform sampler2D blurSrc3;
uniform sampler2D blurSrc4;
uniform sampler2D blurSrc5;

varying vec2 texCoord;

void main()
{
    vec4 color = texture2D(src, texCoord) * _23.blurWeight1.x;
    color += (texture2D(blurSrc1, texCoord) * _23.blurWeight1.y);
    color += (texture2D(blurSrc2, texCoord) * _23.blurWeight1.z);
    color += (texture2D(blurSrc3, texCoord) * _23.blurWeight1.w);
    color += (texture2D(blurSrc4, texCoord) * _23.blurWeight2.x);
    color += (texture2D(blurSrc5, texCoord) * _23.blurWeight2.y);
    vec4 _82 = color;
    float _87 = color.w;
    float _98 = color.w;
    vec3 _101 = ((_82.xyz - vec3(0.5 * _87)) * (1.0 + _23.contrast)) + vec3(0.5 * _98);
    color.x = _101.x;
    color.y = _101.y;
    color.z = _101.z;
    float _112 = color.w;
    vec4 _114 = color;
    vec3 _117 = _114.xyz + vec3(_23.brightness * _112);
    color.x = _117.x;
    color.y = _117.y;
    color.z = _117.z;
    float gray = dot(color.xyz, vec3(0.2989999949932098388671875, 0.58700001239776611328125, 0.114000000059604644775390625));
    vec4 _142 = color;
    vec3 _148 = ((_23.colorizationColor.xyz * gray) * _23.colorizationColor.w) + (_142.xyz * (1.0 - _23.colorizationColor.w));
    color.x = _148.x;
    color.y = _148.y;
    color.z = _148.z;
    vec4 _157 = color;
    vec3 _164 = mix(vec3(gray), _157.xyz, vec3(1.0 + _23.saturation));
    color.x = _164.x;
    color.y = _164.y;
    color.z = _164.z;
    gl_FragData[0] = color * _23.qt_Opacity;
}

