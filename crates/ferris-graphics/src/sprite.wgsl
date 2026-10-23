// The package can provide drawing data, never shader programs.
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;

struct Pixel {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec4<f32>,
}

@vertex
fn vertex_main(
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) tint: vec4<f32>,
) -> Pixel {
    return Pixel(vec4<f32>(position, 0.0, 1.0), uv, tint);
}

@fragment
fn fragment_main(pixel: Pixel) -> @location(0) vec4<f32> {
    return textureSample(image, image_sampler, pixel.uv) * pixel.tint;
}
