#version 120

struct buf
{
    mat4 matrix;
    float opacity;
    float mix_amount;
    float highlight_amount;
    float desaturate_amount;
    vec4 mask_color;
    vec2 viewport_half;
    vec2 inv_viewport_double;
};

uniform buf uniforms;

uniform sampler2D texture_source_0;

varying vec2 uv0;

vec4 texture_color(sampler2D texture_source, vec2 uv)
{
    vec4 color = texture2D(texture_source, uv);
    color = vec4(uniforms.mask_color.xyz * color.w, color.w);
    float gamma = 1.0 / ((2.0 * max(0.25, uniforms.highlight_amount * color.w)) + 0.5);
    vec4 _62 = color;
    vec3 _66 = pow(_62.xyz, vec3(gamma));
    color.x = _66.x;
    color.y = _66.y;
    color.z = _66.z;
    color = mix(color, vec4(vec3(dot(color.xyz, vec3(0.2125999927520751953125, 0.715200006961822509765625, 0.072200000286102294921875))) * 0.5, color.w * 0.5), vec4(uniforms.desaturate_amount));
    return color;
}

void main()
{
    vec2 param = uv0;
    vec4 col = texture_color(texture_source_0, param);
    gl_FragData[0] = col * uniforms.opacity;
}

