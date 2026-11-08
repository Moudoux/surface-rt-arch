#version 120

struct buf
{
    mat4 matrix;
    vec2 textureScale;
    vec4 color;
    float alphaMin;
    float alphaMax;
    float fontScale;
    vec4 vecDelta;
};

uniform buf _71;

uniform sampler2D _qt_texture;

varying vec3 sampleFarLeft;
varying vec3 sampleNearLeft;
varying vec2 sampleCoord;
varying vec3 sampleNearRight;
varying vec3 sampleFarRight;

void main()
{
    vec4 n;
    n.x = texture2DProj(_qt_texture, sampleFarLeft).w;
    n.y = texture2DProj(_qt_texture, sampleNearLeft).w;
    float c = texture2D(_qt_texture, sampleCoord).w;
    n.z = texture2DProj(_qt_texture, sampleNearRight).w;
    n.w = texture2DProj(_qt_texture, sampleFarRight).w;
    vec2 d = min(abs(n.yw - n.xz) * 2.0, vec2(0.670000016689300537109375));
    vec2 lo = mix(vec2(_71.alphaMin), vec2(0.5), d);
    vec2 hi = mix(vec2(_71.alphaMax), vec2(0.5), d);
    n = smoothstep(lo.xxyy, hi.xxyy, n);
    c = smoothstep(lo.x + lo.y, hi.x + hi.y, 2.0 * c);
    gl_FragData[0] = vec4(((n.xyz + n.yzw) + vec3(c)) * 0.333000004291534423828125, c) * _71.color.w;
}

