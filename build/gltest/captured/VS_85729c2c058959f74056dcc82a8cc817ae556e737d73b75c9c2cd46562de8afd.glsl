#version 120

struct buf
{
    mat4 vertexTransform;
    mat3 textureTransform;
    float opacity;
    int textureSwizzle;
};

uniform buf _17;

varying vec2 v_texcoord;
attribute vec2 texcoord;
attribute vec3 position;

void main()
{
    v_texcoord = (_17.textureTransform * vec3(texcoord, 1.0)).xy;
    gl_Position = _17.vertexTransform * vec4(position, 1.0);
}

