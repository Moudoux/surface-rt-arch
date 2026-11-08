#extension GL_OES_EGL_image_external : require
#extension GL_OES_standard_derivatives : enable
precision highp float;
#define sampler2D highp sampler2D
#define sampler3D highp sampler3D
vec4 texture(in sampler2D sampler, in vec2 coordinates) {
    return texture2D(sampler, coordinates);
}
#define TRAIT_MAP_TEXTURE 1
#define TRAIT_UNIFORM_COLOR 0
#define TRAIT_MODULATE 0
#define TRAIT_ADJUST_SATURATION 0
#define TRAIT_TRANSFORM_COLORSPACE 0
#define TRAIT_MAP_EXTERNAL_TEXTURE 0
#define TRAIT_MAP_MULTI_PLANE_TEXTURE 0
#define TRAIT_ROUNDED_CORNERS 0
#define TRAIT_BORDER 0


uniform sampler2D texUnit;
uniform vec2 noiseTextureSize;

varying vec2 uv;

#define fragColor gl_FragColor

void main(void)
{
    vec2 uvNoise = vec2(gl_FragCoord.xy / noiseTextureSize);

    fragColor = vec4(texture(texUnit, uvNoise).rrr, 0);
}

