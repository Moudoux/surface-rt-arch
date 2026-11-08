#version 120

struct buf
{
    mat4 matrix;
    float opacity;
    float size;
    float borderWidth;
    vec2 aspect;
    vec2 offset;
    vec4 radius;
    vec4 color;
    vec4 shadowColor;
    vec4 borderColor;
};

uniform buf ubuf;

uniform sampler2D textureSource;

varying vec2 uv;

float sdf_rounded_rectangle(vec2 point, vec2 rect, inout vec4 radius)
{
    vec2 _39;
    if (point.x > 0.0)
    {
        _39 = radius.xy;
    }
    else
    {
        _39 = radius.zw;
    }
    radius.x = _39.x;
    radius.y = _39.y;
    float _56;
    if (point.y > 0.0)
    {
        _56 = radius.x;
    }
    else
    {
        _56 = radius.y;
    }
    radius.x = _56;
    vec2 d = (abs(point) - rect) + vec2(radius.x);
    return (min(max(d.x, d.y), 0.0) + length(max(d, vec2(0.0)))) - radius.x;
}

vec4 sdf_render(float sdf, vec4 sourceColor, vec4 sdfColor, float alpha, float smoothing)
{
    float g = fwidth(sdf);
    return mix(sourceColor, sdfColor, vec4(alpha * (1.0 - smoothstep((-smoothing) * g, smoothing * g, sdf))));
}

vec4 sdf_render(float sdf, vec4 sourceColor, vec4 sdfColor)
{
    float param = sdf;
    vec4 param_1 = sourceColor;
    vec4 param_2 = sdfColor;
    float param_3 = 1.0;
    float param_4 = 0.625;
    return sdf_render(param, param_1, param_2, param_3, param_4);
}

void main()
{
    vec4 clamped_radius = clamp(ubuf.radius * 2.0, vec4(0.0), vec4(1.0));
    vec4 col = vec4(0.0);
    float inverse_scale = 1.0 / ((1.0 + ubuf.size) + (length(ubuf.offset) * 2.0));
    vec4 size_factor = (vec4(0.0500000007450580596923828125) / max(clamped_radius, vec4(0.0500000007450580596923828125))) * 0.5;
    vec4 shadow_radius = clamped_radius + (size_factor * ubuf.size);
    vec2 param = uv - ((ubuf.offset * 2.0) * inverse_scale);
    vec2 param_1 = ubuf.aspect * inverse_scale;
    vec4 param_2 = shadow_radius * inverse_scale;
    float _193 = sdf_rounded_rectangle(param, param_1, param_2);
    float shadow = _193;
    col = mix(col, ubuf.shadowColor * sign(ubuf.size), vec4(1.0 - smoothstep((-ubuf.size) * 0.5, ubuf.size * 0.5, shadow)));
    vec4 corner_radius = clamped_radius * inverse_scale;
    vec2 param_3 = uv;
    vec2 param_4 = ubuf.aspect * inverse_scale;
    vec4 param_5 = corner_radius;
    float _228 = sdf_rounded_rectangle(param_3, param_4, param_5);
    float inner_rect = _228;
    vec2 texture_uv = ((uv / ubuf.aspect) + vec2(1.0 * inverse_scale)) / vec2(2.0 * inverse_scale);
    vec4 texture_color = texture2D(textureSource, texture_uv);
    vec4 shape_color = mix(ubuf.color, texture_color, vec4(texture_color.w));
    float param_6 = inner_rect;
    vec4 param_7 = col;
    vec4 param_8 = shape_color;
    col = sdf_render(param_6, param_7, param_8);
    gl_FragData[0] = col * ubuf.opacity;
}

