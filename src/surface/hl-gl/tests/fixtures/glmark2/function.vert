#if defined(GL_ES)
#define HIGHP_OR_DEFAULT highp
#else
#define HIGHP_OR_DEFAULT
#endif
#if defined(GL_ES)
#define MEDIUMP_OR_DEFAULT mediump
#else
#define MEDIUMP_OR_DEFAULT
#endif
attribute vec3 position;

uniform mat4 ModelViewProjectionMatrix;

// Removing this varying causes an inexplicable performance regression
// with r600g... Keeping it for now.
varying vec4 dummy;

float process(float d)
{
        d = fract(3.0 * d) * fract(3.0 * d);
    d = fract(sqrt(d));

    return d;
}

void main(void)
{
    dummy = vec4(1.0);

    float d = fract(position.x);

        d = process(d);


    vec4 pos = vec4(position.x,
                    position.y + 0.1 * d * fract(position.x),
                    position.z, 1.0);

    // Transform the position to clip coordinates
    gl_Position = ModelViewProjectionMatrix * pos;
}

