#version 120

struct buf
{
    mat4 qt_Matrix;
    float qt_Opacity;
    vec4 colorBorder;
};

uniform buf _62;

uniform sampler2D source;

varying vec2 qt_TexCoord0;

void main()
{
    vec4 colorSource = texture2D(source, qt_TexCoord0);
    vec2 m = qt_TexCoord0 - vec2(0.5);
    float dist = sqrt((m.x * m.x) + (m.y * m.y));
    if (dist < 0.4699999988079071044921875)
    {
        gl_FragData[0] = colorSource;
    }
    else
    {
        if (dist < 0.4799999892711639404296875)
        {
            gl_FragData[0] = mix(colorSource, _62.colorBorder, vec4((dist - 0.4699999988079071044921875) / 0.00999999977648258209228515625));
        }
        else
        {
            if (dist < 0.4900000095367431640625)
            {
                gl_FragData[0] = _62.colorBorder;
            }
            else
            {
                if (dist < 0.5)
                {
                    gl_FragData[0] = mix(_62.colorBorder, vec4(0.0), vec4((dist - 0.4900000095367431640625) / 0.00999999977648258209228515625));
                }
                else
                {
                    gl_FragData[0] = vec4(0.0);
                }
            }
        }
    }
    gl_FragData[0] *= _62.qt_Opacity;
}

