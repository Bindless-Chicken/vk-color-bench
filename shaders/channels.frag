#version 450

layout(set = 0, binding = 0) uniform sampler2D source_image;
layout(push_constant) uniform ChannelSettings {
    uint channel;
} settings;
layout(location = 0) out vec4 output_color;

void main() {
    vec4 source = texelFetch(source_image, ivec2(gl_FragCoord.xy), 0);
    output_color = vec4(vec3(source[settings.channel]), 1.0);
}
