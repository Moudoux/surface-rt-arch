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

varying vec2 uv0;
attribute vec2 in_uv0;
attribute vec4 in_vertex;
attribute float _qt_order;

void main()
{
    uv0 = in_uv0;
    vec4 position = uniforms.matrix * in_vertex;
    vec2 pixel = (((position.xy / vec2(position.w)) * uniforms.viewport_half) + uniforms.viewport_half) + vec2(9.9999997473787516355514526367188e-05);
    float _61 = position.w;
    vec2 _62 = ((floor(pixel + vec2(0.5)) * uniforms.inv_viewport_double) - vec2(1.0)) * _61;
    position.x = _62.x;
    position.y = _62.y;
    gl_Position = position;
    gl_Position.z = _qt_order * gl_Position.w;
}

