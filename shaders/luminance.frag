#version 450

layout(set = 0, binding = 0) uniform sampler2D source_image;
layout(location = 0) out vec4 output_color;

vec3 decode_pq(vec3 encoded) {
    vec3 p = pow(clamp(encoded, 0.0, 1.0), vec3(32.0 / 2523.0));
    return pow(max(p - 3424.0 / 4096.0, 0.0)
        / (2413.0 / 128.0 - 2392.0 / 128.0 * p), vec3(16384.0 / 2610.0));
}

vec3 encode_pq(vec3 linear) {
    vec3 p = pow(clamp(linear, 0.0, 1.0), vec3(2610.0 / 16384.0));
    return pow((3424.0 / 4096.0 + 2413.0 / 128.0 * p)
        / (1.0 + 2392.0 / 128.0 * p), vec3(2523.0 / 32.0));
}

vec3 palette(float t) {
    // Keep in sync with luminance_legend in ui.rs.
    const vec3 colors[9] = vec3[9](
        vec3(0, 0, 20), vec3(25, 40, 100), vec3(0, 100, 180),
        vec3(0, 175, 190), vec3(0, 180, 45), vec3(235, 230, 0),
        vec3(255, 65, 0), vec3(255, 170, 120), vec3(255, 240, 220)
    );
    float x = clamp(t, 0.0, 1.0) * 8.0;
    int i = min(int(x), 7);
    return mix(colors[i], colors[i + 1], x - float(i)) / 255.0;
}

void main() {
    vec4 source = texelFetch(source_image, ivec2(gl_FragCoord.xy), 0);
    float luminance = dot(decode_pq(source.rgb), vec3(0.2627, 0.6780, 0.0593));
    vec3 srgb = palette(encode_pq(vec3(luminance)).r);
    vec3 linear = mix(srgb / 12.92, pow((srgb + 0.055) / 1.055, vec3(2.4)),
        greaterThan(srgb, vec3(0.04045)));
    vec3 rec2020 = mat3(
        0.627404, 0.069097, 0.016391,
        0.329283, 0.919540, 0.088013,
        0.043313, 0.011362, 0.895595
    ) * linear;
    output_color = vec4(encode_pq(rec2020 * (203.0 / 10000.0)), source.a);
}
