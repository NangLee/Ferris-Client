//! Converts a bounded media recipe into a fixed FFmpeg command.
//! The scene cannot supply command line switches, protocols, or executable code.

use anyhow::{Context, Result, bail, ensure};
use ferris_scene::{FileMode, Manifest, MediaInput, validate_media_path};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const INPUT_FORMATS: &str = "rawvideo,png_pipe,jpeg_pipe,webp_pipe,bmp_pipe,gif,image2,image2pipe,apng,mov,matroska,webm,avi,mp3,wav,ogg,flac,aac";
const FILTERS: &[&str] = &[
    "scale",
    "crop",
    "eq",
    "boxblur",
    "format",
    "pad",
    "split",
    "loop",
    "settb",
    "setpts",
    "blend",
    "fps",
    "color",
    "fade",
    "null",
    "overlay",
    "drawtext",
    "atrim",
    "asetpts",
    "aresample",
    "adelay",
    "asetrate",
    "atempo",
    "volume",
    "amix",
    "apad",
    "afade",
    "anull",
    "aformat",
    "nullsink",
    "anullsink",
];

pub fn file(root: &Path, relative: &str) -> Result<PathBuf> {
    validate_media_path(relative)?;
    ensure!(
        relative
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || b"/._-".contains(&value)),
        "Unsupported scene asset filename"
    );
    checked_file(root, relative)
}

pub fn checked_file(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path).context("Missing scene asset")?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Scene assets must be regular files"
    );
    let canonical = path.canonicalize()?;
    ensure!(
        canonical.starts_with(root),
        "Scene asset is outside its package"
    );
    Ok(canonical)
}

pub fn command(root: &Path, output: &Path, ffmpeg: &Path, manifest: &Manifest) -> Result<Command> {
    let filter_graph = validate_graph(manifest)?;
    let mut command = Command::new(ffmpeg);
    command.current_dir(root).args([
        "-hide_banner",
        "-loglevel",
        "warning",
        "-nostdin",
        "-y",
        "-filter_threads",
        "2",
        "-filter_complex_threads",
        "2",
    ]);
    for input in &manifest.media.inputs {
        command.args([
            "-protocol_whitelist",
            "file,pipe",
            "-format_whitelist",
            INPUT_FORMATS,
        ]);
        match input {
            MediaInput::Frames => {
                command
                    .args([
                        "-thread_queue_size",
                        "4",
                        "-f",
                        "rawvideo",
                        "-pixel_format",
                        "rgba",
                        "-video_size",
                    ])
                    .arg(format!("{}x{}", manifest.width, manifest.height))
                    .args(["-framerate", &manifest.fps.to_string(), "-i", "pipe:0"]);
            }
            MediaInput::File { path, mode } => {
                file(root, path)?;
                match mode {
                    FileMode::Once => {}
                    FileMode::Still => {
                        command.args(["-loop", "1", "-framerate", &manifest.fps.to_string()]);
                    }
                    FileMode::Repeat => {
                        command.args(["-stream_loop", "-1"]);
                    }
                }
                // Paths are validated against the canonical package root above.
                // Keep FFmpeg input names portable, including on Windows where
                // canonicalize() produces an extended Win32 path prefix.
                command.arg("-i").arg(path);
            }
        }
    }
    for font in &manifest.media.fonts {
        file(root, font)?;
    }
    command.args([
        "-filter_complex",
        &filter_graph,
        "-map",
        &manifest.media.video_map,
    ]);
    if let Some(audio) = &manifest.media.audio_map {
        command.args(["-map", audio, "-c:a", "aac", "-b:a", "192k"]);
    } else {
        command.arg("-an");
    }
    command
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "20",
            "-threads",
            "2",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
            "-t",
        ])
        .arg(format!("{:.6}", manifest.media.duration_seconds))
        .arg(output);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    Ok(command)
}

fn validate_graph(manifest: &Manifest) -> Result<String> {
    let graph = &manifest.media.filter_graph;
    ensure!(
        !graph.contains(['\0', '\r', '\n']),
        "Invalid media graph control characters"
    );
    let chains = split(graph, ';')?;
    let mut filter_count = 0;
    for chain in chains {
        for filter in split(chain, ',')? {
            filter_count += 1;
            ensure!(filter_count <= 512, "Media filter budget exceeded");
            let mut body = filter.trim();
            while body.starts_with('[') {
                let end = body.find(']').context("Invalid media input label")?;
                ensure!(end <= 80, "Invalid media input label");
                ensure!(
                    body[1..end]
                        .bytes()
                        .all(|value| value.is_ascii_alphanumeric() || b"_:".contains(&value)),
                    "Invalid media input label"
                );
                body = body[end + 1..].trim_start();
            }
            let name_end = body
                .bytes()
                .take_while(|value| value.is_ascii_alphanumeric() || *value == b'_')
                .count();
            let name = &body[..name_end];
            ensure!(FILTERS.contains(&name), "Unsupported media filter: {name}");
            let options = body[name_end..].strip_prefix('=').unwrap_or("");
            let mut font_found = false;
            let mut expansion_none = false;
            for option in split(options, ':')? {
                let Some((key, value)) = option.split_once('=') else {
                    continue;
                };
                let key = key.trim();
                ensure!(
                    ![
                        "filename", "file", "textfile", "commands", "command", "model", "load",
                        "font"
                    ]
                    .contains(&key),
                    "Media graph cannot read external files"
                );
                if key == "fontfile" {
                    ensure!(name == "drawtext", "Unexpected font file option");
                    let value = value.trim();
                    ensure!(
                        value.starts_with('\'') && value.ends_with('\''),
                        "Font placeholders must be quoted"
                    );
                    let value = value.trim_matches('\'');
                    let number = value
                        .strip_prefix("@font/")
                        .context("Fonts must reference a packaged font placeholder")?;
                    let index: usize = number.parse().context("Invalid font placeholder")?;
                    ensure!(
                        index < manifest.media.fonts.len() && number == index.to_string(),
                        "Invalid font placeholder"
                    );
                    font_found = true;
                }
                if key == "expansion" {
                    ensure!(value == "none", "Text expansion is not permitted");
                    expansion_none = true;
                }
            }
            if name == "drawtext" {
                ensure!(
                    font_found && expansion_none,
                    "Drawtext requires a packaged font and expansion=none"
                );
            }
        }
    }
    let mut graph = graph.clone();
    // Longest placeholder first prevents @font/1 from matching @font/10.
    for index in (0..manifest.media.fonts.len()).rev() {
        graph = graph.replace(
            &format!("fontfile='@font/{index}'"),
            &format!("fontfile='{}'", manifest.media.fonts[index]),
        );
    }
    Ok(graph)
}

/// FFmpeg quotes and escapes delimiters inside text and expression arguments.
fn split(value: &str, delimiter: char) -> Result<Vec<&str>> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (offset, character) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == '\'' {
            quoted = !quoted;
            continue;
        }
        if !quoted && character == delimiter {
            parts.push(&value[start..offset]);
            start = offset + character.len_utf8();
        }
    }
    if quoted || escaped {
        bail!("Unterminated media graph quote or escape");
    }
    parts.push(&value[start..]);
    Ok(parts)
}
