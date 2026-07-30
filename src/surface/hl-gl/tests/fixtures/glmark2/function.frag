#if defined(GL_ES) && defined(GL_FRAGMENT_PRECISION_HIGH)
#define HIGHP_OR_DEFAULT highp
#else
#define HIGHP_OR_DEFAULT
#endif
#if defined(GL_ES)
#define MEDIUMP_OR_DEFAULT mediump
#else
#define MEDIUMP_OR_DEFAULT
#endif
#ifdef GL_ES
precision mediump float;
#endif
varying vec4 dummy;

float process(float d)
{
    d = fract(3.0 * d) * fract(3.0 * d);
    d = fract(sqrt(d));

    return d;
}

void main(void)
{
    // should be declared highp since the multiplication can overflow in
    // mediump, particularly if mediump is implemented as fp16
    HIGHP_OR_DEFAULT vec2 FragCoord = gl_FragCoord.xy;
    float d = fract(FragCoord.x * FragCoord.y * 0.0001);

    d = process(d);


    gl_FragColor = vec4(d, d, d, 1.0);
}
