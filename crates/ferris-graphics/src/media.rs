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
const TARGET_VIDEO_BYTES: u64 = 268 * 1024 * 1024;
const AUDIO_BITRATE: u64 = 192_000;
const MAX_VIDEO_BITRATE: u64 = 50_000_000;
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

pub fn command(
    root: &Path,
    output: &Path,
    ffmpeg: &Path,
    manifest: &Manifest,
    prefer_hardware: bool,
) -> Result<Command> {
    let validated_graph = validate_graph(manifest)?;
    let video_bitrate = video_bitrate_limit(
        manifest.width,
        manifest.height,
        manifest.fps,
        manifest.media.duration_seconds,
        manifest.media.audio_map.is_some(),
    )?;
    let rate = video_bitrate.to_string();
    let buffer = (video_bitrate * 2).to_string();
    eprintln!(
        "Video rate limit: {:.2} Mb/s (268 MiB output target)",
        video_bitrate as f64 / 1_000_000.0
    );
    let (mut filter_graph, cached_background) = cache_still_background(manifest, &validated_graph);
    if cached_background {
        eprintln!("Media filter: cached still background and intro blur");
    }
    let audio_map = if let Some(source) = manifest.media.audio_map.as_deref() {
        const LIMITED_AUDIO: &str = "[ferris_audio_limited]";
        ensure!(
            !filter_graph.contains(LIMITED_AUDIO),
            "Scene uses the reserved audio output label"
        );
        // Juz's media recipe may pad silence indefinitely. Bound audio by
        // sample count, then assign timestamps from the resulting samples.
        // This avoids FFmpeg overflowing undefined packet timestamps when
        // the audio graph reaches its end before the video pipe finishes.
        let audio_samples = (manifest.media.duration_seconds * 48_000.0).ceil() as u64;
        filter_graph.push_str(&format!(
            ";{source}aresample=48000,atrim=end_sample={audio_samples},asetpts=N/SR/TB{LIMITED_AUDIO}"
        ));
        Some(LIMITED_AUDIO)
    } else {
        None
    };
    let filter_threads = std::thread::available_parallelism()
        .map_or(1, |count| count.get())
        .to_string();
    let mut command = Command::new(ffmpeg);
    command.current_dir(root).args([
        "-hide_banner",
        "-loglevel",
        "warning",
        "-nostdin",
        "-y",
        "-filter_threads",
        &filter_threads,
        "-filter_complex_threads",
        &filter_threads,
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
    if let Some(audio) = audio_map {
        command.args(["-map", audio, "-c:a", "aac", "-b:a", "192k"]);
    } else {
        command.arg("-an");
    }
    let nvenc = prefer_hardware && nvenc_available(ffmpeg, manifest.width, manifest.height);
    if nvenc {
        eprintln!("Video encoder: h264_nvenc (p4)");
        command.args([
            "-c:v",
            "h264_nvenc",
            "-preset",
            "p4",
            "-rc",
            "vbr",
            "-cq",
            "20",
            "-b:v",
            &rate,
        ]);
    } else {
        eprintln!("Video encoder: libx264 (veryfast)");
        command.args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "20"]);
    }
    command.args(["-maxrate", &rate, "-bufsize", &buffer]);
    command
        .args([
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

fn video_bitrate_limit(
    width: u32,
    height: u32,
    fps: u32,
    duration_seconds: f64,
    has_audio: bool,
) -> Result<u64> {
    ensure!(
        duration_seconds.is_finite() && duration_seconds > 0.0,
        "Invalid scene duration"
    );
    let audio_bitrate = if has_audio { AUDIO_BITRATE } else { 0 };
    let available = ((TARGET_VIDEO_BYTES as f64 * 8.0 / duration_seconds) - audio_bitrate as f64)
        .floor()
        .min(MAX_VIDEO_BITRATE as f64);
    let quality_floor = (width as f64 * height as f64 * fps as f64 * 0.035)
        .ceil()
        .max(1_500_000.0);
    ensure!(
        available >= quality_floor,
        "result_too_large: this scene cannot fit the community upload limit at usable video quality"
    );
    Ok(available as u64)
}

fn nvenc_available(ffmpeg: &Path, width: u32, height: u32) -> bool {
    let source = format!("color=c=black:s={width}x{height}:r=1");
    Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            &source,
            "-frames:v",
            "1",
            "-c:v",
            "h264_nvenc",
            "-preset",
            "p4",
            "-f",
            "null",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn cache_still_background(manifest: &Manifest, graph: &str) -> (String, bool) {
    if graph.contains("[bg_clear_static]") || graph.contains("[bg_soft_static]") {
        return (graph.to_owned(), false);
    }
    if !matches!(
        manifest.media.inputs.first(),
        Some(MediaInput::File {
            mode: FileMode::Still,
            ..
        })
    ) {
        return (graph.to_owned(), false);
    }

    // Juz describes a still image as a video stream when the intro or results
    // fades are present. FFmpeg then scales and blurs the same image for every
    // frame. Cache those two stills before their time-dependent effects.
    const FADE_TO_SPLIT: &str = "[bgsharp];[bgsharp]split[bg_clear][bg_intro_source];[bg_intro_source]boxblur=20:2,eq=brightness=-0.22[bg_soft];[bg_clear][bg_soft]blend=all_expr='";
    let Some(after_input) = graph.strip_prefix("[0:v]") else {
        return (graph.to_owned(), false);
    };
    let Some((base, after_base)) = after_input.split_once(",fade=") else {
        return (graph.to_owned(), false);
    };
    let Some((fade, after_fade)) = after_base.split_once(FADE_TO_SPLIT) else {
        return (graph.to_owned(), false);
    };
    if !fade.starts_with("t=out:st=") || !fade.ends_with(":color=black") {
        return (graph.to_owned(), false);
    }
    let Some((blend, remaining)) = after_fade.split_once("'[bg];") else {
        return (graph.to_owned(), false);
    };
    let Some((intro_start, intro_end, intro_duration)) = juz_intro_blend(blend) else {
        return (graph.to_owned(), false);
    };
    let loop_still = format!(
        "loop=loop=-1:size=1:start=0,settb=expr=1/{},setpts=N",
        manifest.fps
    );
    let optimized = format!(
        "[0:v]{base},split[bg_clear_static][bg_soft_static];\
         [bg_clear_static]{loop_still},fade={fade}[bg_clear];\
         [bg_soft_static]boxblur=20:2,eq=brightness=-0.22,format=rgba,{loop_still},fade=t=out:st={intro_start}:d={intro_duration}:alpha=1[bg_soft];\
         [bg_clear][bg_soft]overlay=shortest=0:format=auto:enable='lt(t,{intro_end})'[bg];{remaining}"
    );
    (optimized, true)
}

fn juz_intro_blend(blend: &str) -> Option<(&str, &str, &str)> {
    let rest = blend.strip_prefix(r"if(lt(T\,")?;
    let (start, rest) = rest.split_once(r"),B,if(lt(T\,")?;
    let (end, rest) = rest.split_once("),A*((T-")?;
    let (repeat_start, rest) = rest.split_once(")/")?;
    let (duration, rest) = rest.split_once(")+B*((")?;
    let (repeat_end, rest) = rest.split_once("-T)/")?;
    let repeat_duration = rest.strip_suffix("),A))")?;
    if start != repeat_start || end != repeat_end || duration != repeat_duration {
        return None;
    }
    let (start_seconds, end_seconds, duration_seconds) = (
        start.parse::<f64>().ok()?,
        end.parse::<f64>().ok()?,
        duration.parse::<f64>().ok()?,
    );
    if !start_seconds.is_finite()
        || !end_seconds.is_finite()
        || !duration_seconds.is_finite()
        || !(0.0..=30.0).contains(&start_seconds)
        || end_seconds > 30.0
        || (end_seconds - start_seconds - duration_seconds).abs() > 0.001
        || duration_seconds <= 0.0
    {
        return None;
    }
    Some((start, end, duration))
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

#[cfg(test)]
mod tests {
    use super::{MAX_VIDEO_BITRATE, TARGET_VIDEO_BYTES, video_bitrate_limit};

    #[test]
    fn six_minute_scene_has_room_for_audio_and_container() {
        let seconds = 22_301.0 / 60.0;
        let rate = video_bitrate_limit(1280, 720, 60, seconds, true).unwrap();
        assert!((5_500_000..6_000_000).contains(&rate));
        let planned_bytes = (rate as f64 + 192_000.0) * seconds / 8.0;
        assert!(planned_bytes <= TARGET_VIDEO_BYTES as f64);
        assert!(planned_bytes < (300 * 1024 * 1024) as f64);
    }

    #[test]
    fn long_scene_rejects_unusable_video_rate_before_rendering() {
        let error = video_bitrate_limit(1920, 1080, 60, 3600.0, true).unwrap_err();
        assert!(error.to_string().starts_with("result_too_large:"));
    }

    #[test]
    fn short_scene_uses_a_reasonable_rate_ceiling() {
        assert_eq!(
            video_bitrate_limit(1280, 720, 60, 30.0, false).unwrap(),
            MAX_VIDEO_BITRATE
        );
    }
}
