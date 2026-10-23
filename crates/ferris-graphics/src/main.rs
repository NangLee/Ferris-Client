mod cpu;
mod gpu;
mod media;

use anyhow::{Context, Result, bail, ensure};
use ferris_scene::{FORMAT, Frame, MAX_TEXTURE_BYTES, Manifest, Record, SceneReader};
use std::{
    collections::HashMap,
    env,
    fs::{self, File},
    io::{BufReader, Write},
    path::PathBuf,
    process::{Child, ExitCode},
    time::Instant,
};

const MAX_VIDEO_BYTES: u64 = 300 * 1024 * 1024;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Ferris drawing worker: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args == ["--capabilities"] {
        println!(
            "{}",
            serde_json::json!({"runtime_kind":"ferris-scene-worker-v1","scene_format":FORMAT,"protocol_version":1,"backends":["gpu","cpu"]})
        );
        return Ok(());
    }
    if args.is_empty() || args == ["--help"] {
        println!(
            "Ferris drawing worker\n\n--scene <DIRECTORY> --out <MP4> [--cpu-renderer] [--ffmpeg <PATH>]\n--capabilities\n\nAccepts prepared drawing scenes only. Replay files and maps are not supported."
        );
        return Ok(());
    }
    let mut scene = None;
    let mut output = None;
    let mut ffmpeg = PathBuf::from("ffmpeg");
    let mut cpu = false;
    let mut index = 0;
    while index < args.len() {
        let flag = &args[index];
        if flag == "--cpu-renderer" {
            ensure!(!cpu, "Duplicate renderer option");
            cpu = true;
            index += 1;
            continue;
        }
        let value = args
            .get(index + 1)
            .context("Missing drawing worker argument")?;
        match flag.as_str() {
            "--scene" => {
                ensure!(scene.is_none(), "Duplicate scene option");
                scene = Some(PathBuf::from(value));
            }
            "--out" => {
                ensure!(output.is_none(), "Duplicate output option");
                output = Some(PathBuf::from(value));
            }
            "--ffmpeg" => ffmpeg = PathBuf::from(value),
            _ => bail!("Unsupported drawing worker option: {flag}"),
        }
        index += 2;
    }
    let scene = scene
        .context("Missing --scene")?
        .canonicalize()
        .context("Scene directory does not exist")?;
    ensure!(scene.is_dir(), "Scene must be a directory");
    let output = output.context("Missing --out")?;
    let output = if output.is_absolute() {
        output
    } else {
        env::current_dir()?.join(output)
    };
    ensure!(
        output
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("mp4")),
        "Output must be an MP4 file"
    );
    if ffmpeg.components().count() > 1 {
        ffmpeg = ffmpeg
            .canonicalize()
            .context("FFmpeg executable does not exist")?;
    }
    let manifest_path = media::checked_file(&scene, "manifest.json")?;
    ensure!(
        fs::metadata(&manifest_path)?.len() <= 2 * 1024 * 1024,
        "Scene manifest is too large"
    );
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(manifest_path)?).context("Invalid scene manifest")?;
    manifest.validate()?;
    let commands = media::checked_file(&scene, "commands.fscene.gz")?;
    ensure!(
        fs::metadata(&commands)?.len() <= 300 * 1024 * 1024,
        "Scene drawing stream is too large"
    );
    let mut reader = SceneReader::new(BufReader::new(File::open(commands)?))?;
    let mut renderer = if cpu {
        Renderer::Cpu(cpu::CpuRenderer::new(manifest.width, manifest.height))
    } else {
        match pollster::block_on(gpu::GpuRenderer::new(manifest.width, manifest.height)) {
            Ok(renderer) => Renderer::Gpu(renderer),
            Err(error) => {
                eprintln!("GPU unavailable ({error:#}); using CPU drawing.");
                Renderer::Cpu(cpu::CpuRenderer::new(manifest.width, manifest.height))
            }
        }
    };
    let mut command = media::command(&scene, &output, &ffmpeg, &manifest)?;
    let child = command.spawn().context("Could not start FFmpeg")?;
    let mut child = ChildGuard(child);
    let mut input = child
        .0
        .stdin
        .take()
        .context("Could not open the video encoder input")?;
    let started = Instant::now();
    let mut textures = HashMap::<u32, usize>::new();
    let mut texture_bytes = 0usize;
    let mut frames = 0u64;
    loop {
        match reader.read_record()? {
            Record::Texture {
                id,
                width,
                height,
                rgba,
            } => {
                let previous = textures.get(&id).copied().unwrap_or(0);
                texture_bytes = texture_bytes
                    .checked_sub(previous)
                    .and_then(|value| value.checked_add(rgba.len()))
                    .context("Texture budget overflow")?;
                ensure!(
                    texture_bytes <= MAX_TEXTURE_BYTES && (textures.len() < 32768 || previous > 0),
                    "Scene texture cache budget exceeded"
                );
                textures.insert(id, rgba.len());
                renderer.texture(id, width, height, rgba);
            }
            Record::DropTexture(id) => {
                if let Some(bytes) = textures.remove(&id) {
                    texture_bytes -= bytes;
                }
                renderer.drop_texture(id);
            }
            Record::Frame(frame) => {
                ensure!(frames < manifest.frame_count, "Too many scene frames");
                input
                    .write_all(renderer.draw(&frame)?)
                    .context("Could not send the drawn frame to FFmpeg")?;
                frames += 1;
                if frames == 1
                    || frames == manifest.frame_count
                    || frames % u64::from(manifest.fps.max(1)) == 0
                {
                    eprintln!("Rendering frame {frames}/{}", manifest.frame_count);
                    ensure!(
                        fs::metadata(&output).map_or(0, |value| value.len()) <= MAX_VIDEO_BYTES,
                        "result_too_large: Community output exceeds 300 MiB"
                    );
                    ensure!(
                        started.elapsed().as_secs() <= 4 * 60 * 60,
                        "Drawing job exceeded the four hour limit"
                    );
                }
            }
            Record::End => break,
        }
    }
    ensure!(
        frames == manifest.frame_count,
        "Scene ended before all frames were drawn"
    );
    drop(input);
    let status = child.0.wait().context("Could not wait for FFmpeg")?;
    ensure!(status.success(), "FFmpeg exited with status {status}");
    ensure!(
        fs::metadata(&output)?.len() <= MAX_VIDEO_BYTES,
        "result_too_large: Community output exceeds 300 MiB"
    );
    eprintln!(
        "Drawing complete: {frames} frames in {:.2}s",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

enum Renderer {
    Cpu(cpu::CpuRenderer),
    Gpu(gpu::GpuRenderer),
}
impl Renderer {
    fn texture(&mut self, id: u32, width: u32, height: u32, rgba: Vec<u8>) {
        match self {
            Self::Cpu(renderer) => renderer.texture(id, width, height, rgba),
            Self::Gpu(renderer) => renderer.texture(id, width, height, &rgba),
        }
    }
    fn drop_texture(&mut self, id: u32) {
        match self {
            Self::Cpu(renderer) => renderer.drop_texture(id),
            Self::Gpu(renderer) => renderer.drop_texture(id),
        }
    }
    fn draw(&mut self, frame: &Frame) -> Result<&[u8]> {
        match self {
            Self::Cpu(renderer) => renderer.draw(frame),
            Self::Gpu(renderer) => renderer.draw(frame),
        }
    }
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
