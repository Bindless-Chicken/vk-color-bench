#version 450

layout(set = 0, binding = 0) uniform sampler2D source_image;
layout(location = 0) out vec4 output_color;

void main() {
    vec4 source = texelFetch(source_image, ivec2(gl_FragCoord.xy), 0);
    if (any(greaterThanEqual(source.rgb, vec3(1.0)))) {
        output_color = vec4(1.0, 0.0, 0.0, source.a);
    } else if (all(lessThanEqual(source.rgb, vec3(0.0)))) {
        output_color = vec4(0.0, 0.0, 1.0, source.a);
    } else {
        output_color = source;
    }
}
