//! A small affine sprite rasterizer. It has no knowledge of game objects.

use anyhow::{Context, Result};
use ferris_scene::{BlendMode, DrawCommand, Frame, Sprite};
use std::collections::HashMap;

pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub struct CpuRenderer {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    textures: HashMap<u32, Texture>,
}

impl CpuRenderer {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
            textures: HashMap::new(),
        }
    }

    pub fn texture(&mut self, id: u32, width: u32, height: u32, rgba: Vec<u8>) {
        self.textures.insert(
            id,
            Texture {
                width,
                height,
                rgba,
            },
        );
    }

    pub fn drop_texture(&mut self, id: u32) {
        self.textures.remove(&id);
    }

    pub fn draw(&mut self, frame: &Frame) -> Result<&[u8]> {
        let clear = frame.clear_color.map(to_byte);
        for pixel in self.pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&clear);
        }
        for command in &frame.commands {
            match command {
                DrawCommand::Sprites {
                    texture,
                    blend,
                    sprites,
                } => {
                    let texture = self
                        .textures
                        .get(texture)
                        .context("Frame refers to an unknown texture")?;
                    for sprite in sprites {
                        draw_sprite(
                            &mut self.pixels,
                            self.width,
                            self.height,
                            texture,
                            sprite,
                            *blend,
                        );
                    }
                }
                DrawCommand::MaskOutsideCircle {
                    center,
                    radius,
                    color,
                } => {
                    let color = color.map(to_byte);
                    let radius_squared = radius * radius;
                    for y in 0..self.height {
                        for x in 0..self.width {
                            let dx = x as f32 - center[0];
                            let dy = y as f32 - center[1];
                            if dx * dx + dy * dy > radius_squared {
                                let offset = (y as usize * self.width as usize + x as usize) * 4;
                                self.pixels[offset..offset + 4].copy_from_slice(&color);
                            }
                        }
                    }
                }
            }
        }
        if frame.output_alpha < 1.0 {
            for pixel in self.pixels.chunks_exact_mut(4) {
                pixel[3] = (f32::from(pixel[3]) * frame.output_alpha).round() as u8;
            }
        }
        Ok(&self.pixels)
    }
}

fn draw_sprite(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    texture: &Texture,
    sprite: &Sprite,
    blend: BlendMode,
) {
    if sprite.size[0] <= 0.0 || sprite.size[1] <= 0.0 {
        return;
    }
    let (sin, cos) = sprite.rotation.sin_cos();
    let pivot = [
        sprite.position[0] + sprite.origin[0],
        sprite.position[1] + sprite.origin[1],
    ];
    let mut left = f32::INFINITY;
    let mut top = f32::INFINITY;
    let mut right = f32::NEG_INFINITY;
    let mut bottom = f32::NEG_INFINITY;
    for point in [
        [0.0, 0.0],
        [sprite.size[0], 0.0],
        sprite.size,
        [0.0, sprite.size[1]],
    ] {
        let x = point[0] - sprite.origin[0];
        let y = point[1] - sprite.origin[1];
        let output_x = pivot[0] + x * cos - y * sin;
        let output_y = pivot[1] + x * sin + y * cos;
        left = left.min(output_x);
        right = right.max(output_x);
        top = top.min(output_y);
        bottom = bottom.max(output_y);
    }
    let left = left.floor().max(0.0).min(width as f32) as u32;
    let top = top.floor().max(0.0).min(height as f32) as u32;
    let right = right.ceil().max(0.0).min(width as f32) as u32;
    let bottom = bottom.ceil().max(0.0).min(height as f32) as u32;
    for y in top..bottom {
        for x in left..right {
            let dx = x as f32 + 0.5 - pivot[0];
            let dy = y as f32 + 0.5 - pivot[1];
            let local_x = (dx * cos + dy * sin + sprite.origin[0]) / sprite.size[0];
            let local_y = (-dx * sin + dy * cos + sprite.origin[1]) / sprite.size[1];
            if !(0.0..1.0).contains(&local_x) || !(0.0..1.0).contains(&local_y) {
                continue;
            }
            let u = sprite.uv_rect[0] + (sprite.uv_rect[2] - sprite.uv_rect[0]) * local_x;
            let v = sprite.uv_rect[1] + (sprite.uv_rect[3] - sprite.uv_rect[1]) * local_y;
            let source_x = (u * texture.width as f32)
                .floor()
                .clamp(0.0, (texture.width - 1) as f32) as usize;
            let source_y = (v * texture.height as f32)
                .floor()
                .clamp(0.0, (texture.height - 1) as f32) as usize;
            let source = (source_y * texture.width as usize + source_x) * 4;
            let color = std::array::from_fn::<_, 4, _>(|channel| {
                f32::from(texture.rgba[source + channel]) / 255.0 * sprite.color[channel]
            });
            let destination = (y as usize * width as usize + x as usize) * 4;
            composite(&mut pixels[destination..destination + 4], color, blend);
        }
    }
}

fn composite(destination: &mut [u8], source: [f32; 4], blend: BlendMode) {
    match blend {
        BlendMode::Copy => {
            for channel in 0..4 {
                destination[channel] = to_byte(source[channel]);
            }
        }
        BlendMode::Alpha => {
            let keep = 1.0 - source[3];
            for channel in 0..3 {
                destination[channel] = to_byte(
                    source[channel] * source[3] + f32::from(destination[channel]) / 255.0 * keep,
                );
            }
            destination[3] = to_byte(source[3] + f32::from(destination[3]) / 255.0 * keep);
        }
        BlendMode::Additive => {
            for channel in 0..3 {
                destination[channel] =
                    to_byte(source[channel] * source[3] + f32::from(destination[channel]) / 255.0);
            }
            destination[3] = to_byte(source[3] + f32::from(destination[3]) / 255.0);
        }
    }
}

fn to_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}
