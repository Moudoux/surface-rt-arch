#version 120

struct buf
{
    mat4 qt_Matrix;
    float qt_Opacity;
    float spread;
    vec2 dirstep;
    vec4 color;
    float thickness;
};

uniform buf _17;

attribute vec4 qt_Vertex;
varying vec2 tL3;
attribute vec2 qt_MultiTexCoord0;
varying vec2 tL2;
varying vec2 tL1;
varying vec2 tL0;
varying vec2 tR0;
varying vec2 tR1;
varying vec2 tR2;
varying vec2 tR3;

void main()
{
    gl_Position = _17.qt_Matrix * qt_Vertex;
    tL3 = qt_MultiTexCoord0 + ((_17.dirstep * _17.spread) * 6.244450092315673828125);
    tL2 = qt_MultiTexCoord0 + ((_17.dirstep * _17.spread) * 4.314050197601318359375);
    tL1 = qt_MultiTexCoord0 + ((_17.dirstep * _17.spread) * 2.3931701183319091796875);
    tL0 = qt_MultiTexCoord0 + ((_17.dirstep * _17.spread) * (-0.4783129990100860595703125));
    tR0 = qt_MultiTexCoord0 + ((_17.dirstep * _17.spread) * 1.0);
    tR1 = qt_MultiTexCoord0 + ((_17.dirstep * _17.spread) * (-2.3931701183319091796875));
    tR2 = qt_MultiTexCoord0 + ((_17.dirstep * _17.spread) * (-4.314050197601318359375));
    tR3 = qt_MultiTexCoord0 + ((_17.dirstep * _17.spread) * (-6.244450092315673828125));
}

