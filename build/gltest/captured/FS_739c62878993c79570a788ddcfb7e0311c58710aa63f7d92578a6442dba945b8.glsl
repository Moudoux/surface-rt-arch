#version 120

struct buf
{
    mat4 vertexTransform;
    mat3 textureTransform;
    float opacity;
    int textureSwizzle;
};

uniform buf _26;

uniform sampler2D textureSampler;

varying vec2 v_texcoord;

void main()
{
    vec4 tmpFragColor = texture2D(textureSampler, v_texcoord);
    tmpFragColor.w *= _26.opacity;
    if (_26.textureSwizzle == 0)
    {
        gl_FragData[0] = tmpFragColor;
    }
    else
    {
        if (_26.textureSwizzle == 2)
        {
            gl_FragData[0] = vec4(tmpFragColor.y, tmpFragColor.z, tmpFragColor.w, tmpFragColor.x);
        }
        else
        {
            gl_FragData[0] = vec4(tmpFragColor.z, tmpFragColor.y, tmpFragColor.x, tmpFragColor.w);
        }
    }
}

