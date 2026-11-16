#version 450

layout(set = 0, binding = 0) uniform sampler2D source_image;
layout(location = 0) out vec4 output_color;

vec3 decode_pq(vec3 encoded) {
    const float m1 = 2610.0 / 16384.0;
    const float m2 = 2523.0 / 32.0;
    const float c1 = 3424.0 / 4096.0;
    const float c2 = 2413.0 / 128.0;
    const float c3 = 2392.0 / 128.0;
    vec3 p = pow(clamp(encoded, 0.0, 1.0), vec3(1.0 / m2));
    return pow(max(p - c1, 0.0) / (c2 - c3 * p), vec3(1.0 / m1));
}

float encode_pq(float linear) {
    const float m1 = 2610.0 / 16384.0;
    const float m2 = 2523.0 / 32.0;
    const float c1 = 3424.0 / 4096.0;
    const float c2 = 2413.0 / 128.0;
    const float c3 = 2392.0 / 128.0;
    float p = pow(clamp(linear, 0.0, 1.0), m1);
    return pow((c1 + c2 * p) / (1.0 + c3 * p), m2);
}

float edge(vec2 a, vec2 b, vec2 p) {
    vec2 ab = b - a;
    vec2 ap = p - a;
    return ab.x * ap.y - ab.y * ap.x;
}

bool in_gamut(vec2 xy, vec2 red, vec2 green, vec2 blue) {
    const float epsilon = 0.000001;
    return edge(red, green, xy) >= -epsilon
        && edge(green, blue, xy) >= -epsilon
        && edge(blue, red, xy) >= -epsilon;
}

void main() {
    vec4 source = texelFetch(source_image, ivec2(gl_FragCoord.xy), 0);
    vec3 rgb = decode_pq(source.rgb);
    vec3 xyz = mat3(
        0.6369580483, 0.2627002120, 0.0000000000,
        0.1446169036, 0.6779980715, 0.0280726930,
        0.1688809752, 0.0593017165, 1.0609850577
    ) * rgb;
    float total = xyz.x + xyz.y + xyz.z;
    vec2 xy = total > 0.0 ? xyz.xy / total : vec2(0.3127, 0.3290);

    float marker = encode_pq(xyz.y);
    if (in_gamut(xy, vec2(0.640, 0.330), vec2(0.300, 0.600), vec2(0.150, 0.060))) {
        output_color = vec4(marker, 0.0, 0.0, source.a);
    } else if (in_gamut(xy, vec2(0.680, 0.320), vec2(0.265, 0.690), vec2(0.150, 0.060))) {
        output_color = vec4(0.0, marker, 0.0, source.a);
    } else {
        output_color = vec4(0.0, 0.0, marker, source.a);
    }
}
