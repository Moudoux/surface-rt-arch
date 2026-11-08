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
#define TRAIT_MODULATE 1
#define TRAIT_ADJUST_SATURATION 1
#define TRAIT_TRANSFORM_COLORSPACE 1
#define TRAIT_MAP_EXTERNAL_TEXTURE 0
#define TRAIT_MAP_MULTI_PLANE_TEXTURE 0
#define TRAIT_ROUNDED_CORNERS 0
#define TRAIT_BORDER 0


attribute vec4 position;

#if TRAIT_MAP_TEXTURE || TRAIT_MAP_EXTERNAL_TEXTURE || TRAIT_MAP_MULTI_PLANE_TEXTURE
attribute vec4 texcoord;
varying vec2 texcoord0;
#endif

#if TRAIT_ROUNDED_CORNERS | TRAIT_BORDER
varying vec2 position0;
#endif

uniform mat4 modelViewProjectionMatrix;

void main()
{
#if TRAIT_MAP_TEXTURE || TRAIT_MAP_EXTERNAL_TEXTURE || TRAIT_MAP_MULTI_PLANE_TEXTURE
    texcoord0 = texcoord.st;
#endif

#if TRAIT_ROUNDED_CORNERS | TRAIT_BORDER
    position0 = position.xy;
#endif

    gl_Position = modelViewProjectionMatrix * position;
}

