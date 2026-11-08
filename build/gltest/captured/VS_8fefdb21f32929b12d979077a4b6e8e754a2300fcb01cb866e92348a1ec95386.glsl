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

uniform buf _17;

varying vec2 sampleCoord;
attribute vec2 tCoord;
attribute vec4 vCoord;
varying vec3 sampleFarLeft;
varying vec3 sampleNearLeft;
varying vec3 sampleNearRight;
varying vec3 sampleFarRight;
attribute float _qt_order;

void main()
{
    sampleCoord = tCoord * _17.textureScale;
    gl_Position = _17.matrix * vCoord;
    vec3 wDelta = _17.vecDelta.xyw * gl_Position.w;
    vec3 farLeft = vCoord.xyw - (wDelta * 0.666999995708465576171875);
    vec3 nearLeft = vCoord.xyw - (wDelta * 0.333000004291534423828125);
    vec3 nearRight = vCoord.xyw + (wDelta * 0.333000004291534423828125);
    vec3 farRight = vCoord.xyw + (wDelta * 0.666999995708465576171875);
    vec2 scale = _17.textureScale / vec2(_17.fontScale);
    vec2 base = sampleCoord - (scale * vCoord.xy);
    sampleFarLeft = vec3((base * farLeft.z) + (scale * farLeft.xy), farLeft.z);
    sampleNearLeft = vec3((base * nearLeft.z) + (scale * nearLeft.xy), nearLeft.z);
    sampleNearRight = vec3((base * nearRight.z) + (scale * nearRight.xy), nearRight.z);
    sampleFarRight = vec3((base * farRight.z) + (scale * farRight.xy), farRight.z);
    gl_Position.z = _qt_order * gl_Position.w;
}

