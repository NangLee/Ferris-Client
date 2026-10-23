//! A drawing format, deliberately independent of replay parsing and game rules.

use anyhow::{Context, Result, bail, ensure};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::{Component, Path},
};

pub const FORMAT: &str = "ferris-scene-v1";
pub const MAGIC: &[u8; 8] = b"FERRIS01";
pub const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_TEXTURE_BYTES: usize = 512 * 1024 * 1024;
pub const MAX_FRAME_COMMANDS: usize = 100_000;
pub const MAX_FRAME_SPRITES: usize = 1_000_000;
pub const MAX_STREAM_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_STREAM_RECORDS: u64 = 2_000_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub frame_count: u64,
    pub media: MediaRecipe,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaRecipe {
    pub inputs: Vec<MediaInput>,
    pub filter_graph: String,
    pub video_map: String,
    pub audio_map: Option<String>,
    pub duration_seconds: f64,
    pub fonts: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MediaInput {
    Frames,
    File { path: String, mode: FileMode },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileMode {
    Once,
    Still,
    Repeat,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub clear_color: [f32; 4],
    pub output_alpha: f32,
    pub commands: Vec<DrawCommand>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DrawCommand {
    Sprites {
        texture: u32,
        blend: BlendMode,
        sprites: Vec<Sprite>,
    },
    MaskOutsideCircle {
        center: [f32; 2],
        radius: f32,
        color: [f32; 4],
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    Alpha,
    Additive,
    Copy,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sprite {
    pub position: [f32; 2],
    pub size: [f32; 2],
    pub uv_rect: [f32; 4],
    pub color: [f32; 4],
    pub origin: [f32; 2],
    pub rotation: f32,
}

impl Manifest {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.format == FORMAT, "Unsupported scene format");
        ensure!(
            (2..=8192).contains(&self.width) && (2..=8192).contains(&self.height),
            "Invalid scene dimensions"
        );
        ensure!(
            self.width % 2 == 0 && self.height % 2 == 0,
            "Video dimensions must be even"
        );
        ensure!(
            u64::from(self.width) * u64::from(self.height) <= 16_777_216,
            "Scene pixel budget exceeded"
        );
        ensure!((1..=240).contains(&self.fps), "Invalid scene frame rate");
        ensure!(
            (1..=864_000).contains(&self.frame_count),
            "Invalid scene frame count"
        );
        let duration = self.media.duration_seconds;
        ensure!(
            duration.is_finite() && duration > 0.0 && duration <= 3600.0,
            "Invalid scene duration"
        );
        ensure!(
            ((duration * f64::from(self.fps)).ceil() - self.frame_count as f64).abs() <= 1.0,
            "Frame count does not match duration"
        );
        ensure!(
            (1..=16).contains(&self.media.inputs.len()),
            "Invalid media input count"
        );
        ensure!(
            self.media
                .inputs
                .iter()
                .filter(|input| matches!(input, MediaInput::Frames))
                .count()
                == 1,
            "A scene needs exactly one frame input"
        );
        for input in &self.media.inputs {
            if let MediaInput::File { path, .. } = input {
                validate_media_path(path)?;
            }
        }
        ensure!(self.media.fonts.len() <= 16, "Too many font files");
        for path in &self.media.fonts {
            validate_media_path(path)?;
        }
        ensure!(
            !self.media.filter_graph.is_empty() && self.media.filter_graph.len() <= 2 * 1024 * 1024,
            "Invalid media filter graph size"
        );
        validate_map(&self.media.video_map)?;
        if let Some(map) = &self.media.audio_map {
            validate_map(map)?;
        }
        Ok(())
    }
}

pub fn validate_media_path(value: &str) -> Result<()> {
    ensure!(
        value.len() <= 240 && value.starts_with("media/"),
        "Media must be inside media/"
    );
    ensure!(
        !value.contains(['\\', ':', '\0', '\n', '\r', '\'', '"']),
        "Invalid media path"
    );
    ensure!(
        Path::new(value)
            .components()
            .all(|part| matches!(part, Component::Normal(_))),
        "Invalid media path components"
    );
    let extension = Path::new(value)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    ensure!(
        ![
            "osr", "osu", "osz", "exe", "dll", "so", "dylib", "sh", "ps1", "bat", "cmd", "desktop"
        ]
        .contains(&extension.as_str()),
        "Source replays, maps, and executables are not scene assets"
    );
    Ok(())
}

fn validate_map(value: &str) -> Result<()> {
    ensure!(
        value.len() >= 3 && value.len() <= 80 && value.starts_with('[') && value.ends_with(']'),
        "Invalid media output label"
    );
    ensure!(
        value[1..value.len() - 1]
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || value == b'_'),
        "Invalid media output label"
    );
    Ok(())
}

impl Frame {
    pub fn validate(&self) -> Result<()> {
        validate_color(&self.clear_color)?;
        ensure!(
            self.output_alpha.is_finite() && (0.0..=1.0).contains(&self.output_alpha),
            "Invalid frame opacity"
        );
        ensure!(
            self.commands.len() <= MAX_FRAME_COMMANDS,
            "Frame command budget exceeded"
        );
        let mut count = 0usize;
        for command in &self.commands {
            match command {
                DrawCommand::Sprites { sprites, .. } => {
                    count = count
                        .checked_add(sprites.len())
                        .context("Sprite count overflow")?;
                    ensure!(count <= MAX_FRAME_SPRITES, "Frame sprite budget exceeded");
                    for sprite in sprites {
                        ensure!(
                            sprite
                                .position
                                .into_iter()
                                .chain(sprite.size)
                                .chain(sprite.origin)
                                .all(|value| value.is_finite() && value.abs() <= 131_072.0),
                            "Invalid sprite geometry"
                        );
                        ensure!(
                            sprite.size[0] >= 0.0 && sprite.size[1] >= 0.0,
                            "Negative sprite size"
                        );
                        ensure!(
                            sprite
                                .uv_rect
                                .iter()
                                .all(|value| value.is_finite() && value.abs() <= 16.0),
                            "Invalid texture coordinates"
                        );
                        ensure!(
                            sprite.rotation.is_finite() && sprite.rotation.abs() <= 1_000_000.0,
                            "Invalid sprite rotation"
                        );
                        validate_color(&sprite.color)?;
                    }
                }
                DrawCommand::MaskOutsideCircle {
                    center,
                    radius,
                    color,
                } => {
                    ensure!(
                        center
                            .iter()
                            .all(|value| value.is_finite() && value.abs() <= 131_072.0),
                        "Invalid mask center"
                    );
                    ensure!(
                        radius.is_finite() && (0.0..=131_072.0).contains(radius),
                        "Invalid mask radius"
                    );
                    validate_color(color)?;
                }
            }
        }
        Ok(())
    }
}

fn validate_color(color: &[f32; 4]) -> Result<()> {
    ensure!(
        color
            .iter()
            .all(|value| value.is_finite() && (0.0..=1.0).contains(value)),
        "Invalid scene color"
    );
    Ok(())
}

pub enum Record {
    Texture {
        id: u32,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    Frame(Frame),
    DropTexture(u32),
    End,
}

pub struct SceneWriter<W: Write> {
    output: GzEncoder<W>,
    bytes: u64,
    records: u64,
}

impl<W: Write> SceneWriter<W> {
    pub fn new(output: W) -> Result<Self> {
        let mut output = GzEncoder::new(output, Compression::fast());
        output.write_all(MAGIC)?;
        Ok(Self {
            output,
            bytes: MAGIC.len() as u64,
            records: 0,
        })
    }

    fn header(&mut self, kind: u8, length: usize) -> Result<()> {
        ensure!(
            length <= MAX_RECORD_BYTES,
            "Scene record exceeds the size limit"
        );
        self.bytes += length as u64 + 5;
        self.records += 1;
        ensure!(
            self.bytes <= MAX_STREAM_BYTES && self.records <= MAX_STREAM_RECORDS,
            "Scene stream budget exceeded"
        );
        self.output.write_all(&[kind])?;
        self.output.write_all(&(length as u32).to_le_bytes())?;
        Ok(())
    }

    pub fn write_texture(&mut self, id: u32, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
        let expected = texture_bytes(width, height)?;
        ensure!(
            rgba.len() == expected,
            "Texture byte count does not match dimensions"
        );
        self.header(1, 12 + rgba.len())?;
        for value in [id, width, height] {
            self.output.write_all(&value.to_le_bytes())?;
        }
        self.output.write_all(rgba)?;
        Ok(())
    }

    pub fn write_frame(&mut self, frame: &Frame) -> Result<()> {
        frame.validate()?;
        let bytes = serde_json::to_vec(frame)?;
        self.header(2, bytes.len())?;
        self.output.write_all(&bytes)?;
        Ok(())
    }

    pub fn drop_texture(&mut self, id: u32) -> Result<()> {
        self.header(3, 4)?;
        self.output.write_all(&id.to_le_bytes())?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<W> {
        self.header(255, 0)?;
        Ok(self.output.finish()?)
    }
}

pub struct SceneReader<R: Read> {
    input: GzDecoder<R>,
    ended: bool,
    bytes: u64,
    records: u64,
}

impl<R: Read> SceneReader<R> {
    pub fn new(input: R) -> Result<Self> {
        let mut input = GzDecoder::new(input);
        let mut magic = [0u8; 8];
        input.read_exact(&mut magic)?;
        ensure!(&magic == MAGIC, "Invalid drawing stream header");
        Ok(Self {
            input,
            ended: false,
            bytes: MAGIC.len() as u64,
            records: 0,
        })
    }

    pub fn read_record(&mut self) -> Result<Record> {
        ensure!(!self.ended, "Drawing stream has already ended");
        let mut header = [0u8; 5];
        self.input
            .read_exact(&mut header)
            .context("Truncated drawing stream")?;
        let length = u32::from_le_bytes(header[1..5].try_into().unwrap()) as usize;
        ensure!(
            length <= MAX_RECORD_BYTES,
            "Drawing record exceeds the size limit"
        );
        self.bytes += length as u64 + 5;
        self.records += 1;
        ensure!(
            self.bytes <= MAX_STREAM_BYTES && self.records <= MAX_STREAM_RECORDS,
            "Drawing stream budget exceeded"
        );
        match header[0] {
            1 => {
                ensure!(length >= 12, "Invalid texture record");
                let mut texture = [0u8; 12];
                self.input.read_exact(&mut texture)?;
                let id = u32::from_le_bytes(texture[0..4].try_into().unwrap());
                let width = u32::from_le_bytes(texture[4..8].try_into().unwrap());
                let height = u32::from_le_bytes(texture[8..12].try_into().unwrap());
                ensure!(
                    texture_bytes(width, height)? == length - 12,
                    "Invalid texture length"
                );
                let mut rgba = vec![0; length - 12];
                self.input.read_exact(&mut rgba)?;
                Ok(Record::Texture {
                    id,
                    width,
                    height,
                    rgba,
                })
            }
            2 => {
                let mut bytes = vec![0; length];
                self.input.read_exact(&mut bytes)?;
                let frame: Frame =
                    serde_json::from_slice(&bytes).context("Invalid drawing frame")?;
                frame.validate()?;
                Ok(Record::Frame(frame))
            }
            3 => {
                ensure!(length == 4, "Invalid texture removal record");
                let mut bytes = [0u8; 4];
                self.input.read_exact(&mut bytes)?;
                Ok(Record::DropTexture(u32::from_le_bytes(bytes)))
            }
            255 => {
                ensure!(length == 0, "Invalid drawing stream terminator");
                let mut extra = [0u8; 1];
                ensure!(
                    self.input.read(&mut extra)? == 0,
                    "Unexpected data after drawing stream"
                );
                self.ended = true;
                Ok(Record::End)
            }
            _ => bail!("Unknown drawing record type"),
        }
    }
}

pub fn texture_bytes(width: u32, height: u32) -> Result<usize> {
    ensure!(
        (1..=8192).contains(&width) && (1..=8192).contains(&height),
        "Invalid texture dimensions"
    );
    let length = width as usize * height as usize * 4;
    ensure!(
        length <= MAX_RECORD_BYTES - 12,
        "Texture exceeds the size limit"
    );
    Ok(length)
}
