use crate::{
    api::Api,
    model::RuntimeInfo,
    storage::{self, Storage},
};
use anyhow::{Context, Result, bail};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};

const MAX_FILES: usize = 100_000;
const MAX_EXPANDED_BYTES: u64 = 5 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReleaseManifest {
    pub runtime_kind: String,
    pub scene_format: String,
    pub release_id: String,
    pub version: String,
    pub platform: String,
    pub artifacts: Artifacts,
    pub expires_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifacts {
    pub renderer: Artifact,
    pub ffmpeg: Artifact,
    pub client: Option<Artifact>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
    pub archive: String,
    pub executable: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledRuntime {
    pub runtime_kind: String,
    pub scene_format: String,
    pub version: String,
    pub root: PathBuf,
    pub renderer: PathBuf,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl InstalledRuntime {
    pub fn info(&self) -> RuntimeInfo {
        RuntimeInfo {
            installed: self.renderer.is_file() && self.ffmpeg.is_file() && self.ffprobe.is_file(),
            installing: false,
            version: Some(self.version.clone()),
            renderer_path: Some(self.renderer.clone()),
            ffmpeg_path: Some(self.ffmpeg.clone()),
            message: "Signed render tools are installed.".into(),
        }
    }
}

pub fn installed(storage: &Storage) -> Option<InstalledRuntime> {
    let runtime: InstalledRuntime = storage::read_json(&storage.data.join("runtime.json")).ok()?;
    let managed_root = storage.data.join("runtimes");
    if !runtime.root.starts_with(&managed_root)
        || runtime.runtime_kind != crate::model::WORKER_RUNTIME_KIND
        || runtime.scene_format != crate::model::SCENE_FORMAT
        || !runtime.renderer.starts_with(&runtime.root)
        || !runtime.renderer.is_file()
        || !runtime.ffmpeg.is_file()
        || !runtime.ffprobe.is_file()
    {
        return None;
    }
    Some(runtime)
}

pub async fn fetch_manifest(api: &Api) -> Result<ReleaseManifest> {
    verify_manifest(api.manifest().await?)
}

fn decode_bytes(text: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(text)
        .or_else(|_| STANDARD.decode(text))
        .context("Invalid base64 in signed release metadata")
}

pub fn verify_manifest(envelope: serde_json::Value) -> Result<ReleaseManifest> {
    let payload = verify_signed_payload(envelope, 256 * 1024)?;
    let manifest: ReleaseManifest = serde_json::from_slice(&payload)?;
    if manifest.runtime_kind != crate::model::WORKER_RUNTIME_KIND
        || manifest.scene_format != crate::model::SCENE_FORMAT
    {
        bail!("This release is not a compatible Ferris scene worker");
    }
    validate_manifest(manifest)
}

pub(crate) fn verify_signed_payload(envelope: serde_json::Value, limit: usize) -> Result<Vec<u8>> {
    let public_key = option_env!("FERRIS_UPDATE_PUBLIC_KEY")
        .filter(|key| !key.is_empty()).context("This build has no pinned update key. Ask the Ferris operator for a signed production build.")?;
    let key_bytes: [u8; 32] = decode_bytes(public_key)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("The pinned update key is invalid"))?;
    let payload = decode_bytes(
        envelope
            .get("payload")
            .and_then(|v| v.as_str())
            .context("Missing signed release payload")?,
    )?;
    if payload.len() > limit {
        bail!("The signed payload is too large");
    }
    let signature = decode_bytes(
        envelope
            .get("signature")
            .and_then(|v| v.as_str())
            .context("Missing release signature")?,
    )?;
    let key = VerifyingKey::from_bytes(&key_bytes)?;
    key.verify_strict(&payload, &Signature::from_slice(&signature)?)
        .context("The release signature does not match this client's pinned key")?;
    Ok(payload)
}

fn validate_manifest(manifest: ReleaseManifest) -> Result<ReleaseManifest> {
    if manifest.platform != crate::model::platform_name() {
        bail!("This signed release targets another operating system");
    }
    if manifest.release_id.is_empty()
        || manifest.release_id.len() > 100
        || !manifest
            .release_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    {
        bail!("Invalid release identifier");
    }
    if let Some(expiry) = &manifest.expires_at {
        let expiry =
            chrono::DateTime::parse_from_rfc3339(expiry).context("Invalid manifest expiry")?;
        if expiry < chrono::Utc::now() {
            bail!("This signed release manifest has expired");
        }
    }
    for artifact in [&manifest.artifacts.renderer, &manifest.artifacts.ffmpeg]
        .into_iter()
        .chain(manifest.artifacts.client.as_ref())
    {
        if artifact.sha256.len() != 64
            || !artifact.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || artifact.bytes == 0
            || artifact.bytes > 2 * 1024 * 1024 * 1024
        {
            bail!("Invalid signed artifact metadata");
        }
        if !matches!(artifact.archive.as_str(), "zip" | "tar.xz" | "none") {
            bail!("Unsupported signed artifact format");
        }
        let url = url::Url::parse(&artifact.url)?;
        if url.scheme() != "https" || url.username() != "" || url.password().is_some() {
            bail!("Signed releases require HTTPS download URLs");
        }
    }
    Ok(manifest)
}

pub async fn install(
    api: &Api,
    storage: &Storage,
    manifest: &ReleaseManifest,
) -> Result<InstalledRuntime> {
    let runtimes = storage.data.join("runtimes");
    fs::create_dir_all(&runtimes)?;
    let release = runtimes.join(format!("{}-{}", manifest.release_id, uuid::Uuid::new_v4()));
    fs::create_dir(&release)?;
    storage::private_directory(&release)?;
    let result = install_in_directory(api, &release, manifest).await;
    match result {
        Ok(runtime) => {
            storage::write_json(&storage.data.join("runtime.json"), &runtime)?;
            Ok(runtime)
        }
        Err(error) => {
            // Only remove the fresh, uniquely named staging directory we own.
            if release.starts_with(&runtimes) {
                let _ = fs::remove_dir_all(&release);
            }
            Err(error)
        }
    }
}

async fn install_in_directory(
    api: &Api,
    release: &Path,
    manifest: &ReleaseManifest,
) -> Result<InstalledRuntime> {
    let renderer_root = release.join("renderer");
    download_artifact(api, &manifest.artifacts.renderer, &renderer_root).await?;
    let renderer = find_executable(
        &renderer_root,
        manifest.artifacts.renderer.executable.as_deref(),
        executable_name("ferris-worker"),
    )?;
    make_executable(&renderer)?;
    check_scene_worker(&renderer, &renderer_root).await?;
    let (ffmpeg, ffprobe) = if let Some(existing) = detect_ffmpeg().await {
        existing
    } else {
        let ffmpeg_root = release.join("ffmpeg");
        download_artifact(api, &manifest.artifacts.ffmpeg, &ffmpeg_root).await?;
        let ffmpeg = find_executable(
            &ffmpeg_root,
            manifest.artifacts.ffmpeg.executable.as_deref(),
            executable_name("ffmpeg"),
        )?;
        let ffprobe = ffmpeg
            .parent()
            .context("Invalid FFmpeg binary path")?
            .join(executable_name("ffprobe"));
        if !ffprobe.is_file() {
            bail!("The signed FFmpeg package does not include ffprobe alongside ffmpeg");
        }
        make_executable(&ffmpeg)?;
        make_executable(&ffprobe)?;
        check_ffmpeg(&ffmpeg, &ffprobe).await?;
        (ffmpeg, ffprobe)
    };
    let root = renderer_root;
    Ok(InstalledRuntime {
        runtime_kind: crate::model::WORKER_RUNTIME_KIND.into(),
        scene_format: crate::model::SCENE_FORMAT.into(),
        version: manifest.version.clone(),
        root,
        renderer,
        ffmpeg,
        ffprobe,
    })
}

async fn download_artifact(api: &Api, artifact: &Artifact, destination: &Path) -> Result<()> {
    fs::create_dir(destination)?;
    let archive_path = destination.join(".download");
    api.download_release(
        &artifact.url,
        &archive_path,
        &artifact.sha256,
        artifact.bytes,
    )
    .await?;
    if artifact.archive == "none" {
        let executable = artifact
            .executable
            .as_deref()
            .context("A single-file artifact needs an executable name")?;
        let target = safe_path(destination, executable)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(archive_path, target)?;
        return Ok(());
    }
    let archive = artifact.archive.clone();
    let source = archive_path.clone();
    let directory = destination.to_path_buf();
    tokio::task::spawn_blocking(move || extract_archive(&source, &directory, &archive)).await??;
    fs::remove_file(archive_path)?;
    Ok(())
}

pub async fn download_client(
    api: &Api,
    storage: &Storage,
    manifest: &ReleaseManifest,
) -> Result<PathBuf> {
    let artifact = manifest
        .artifacts
        .client
        .as_ref()
        .context("This release has no client installer")?;
    if artifact.archive != "none" {
        bail!("Client updates must be signed single-file installers, opened by you");
    }
    let name = artifact
        .executable
        .as_deref()
        .context("Missing client installer name")?;
    if name.contains('/') || name.contains('\\') {
        bail!("Invalid client installer name");
    }
    let directory = storage.cache.join(format!(
        "update-{}-{}",
        manifest.release_id,
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&directory)?;
    let target = safe_path(&directory, name)?;
    api.download_release(&artifact.url, &target, &artifact.sha256, artifact.bytes)
        .await?;
    #[cfg(target_os = "linux")]
    if target
        .extension()
        .is_some_and(|extension| extension == "AppImage")
    {
        make_executable(&target)?;
    }
    Ok(target)
}

pub fn safe_path(root: &Path, name: &str) -> Result<PathBuf> {
    if name.len() > 1024 || name.contains('\0') || name.contains(':') {
        bail!("Unsafe archive path");
    }
    let normalized = name.replace('\\', "/");
    let path = Path::new(&normalized);
    let mut target = root.to_path_buf();
    let mut count = 0;
    for component in path.components() {
        if component == Component::CurDir {
            continue;
        }
        let Component::Normal(part) = component else {
            bail!("Archive paths must stay inside their installation directory");
        };
        let part_text = part.to_string_lossy();
        if part_text == ".env"
            || part_text.starts_with(".env.")
            || part_text.ends_with('.')
            || part_text.ends_with(' ')
        {
            bail!("Environment files and ambiguous filenames are not allowed in runtime packages");
        }
        let stem = part_text
            .split('.')
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        if matches!(
            stem.as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        ) {
            bail!("Reserved archive filename");
        }
        target.push(part);
        count += 1;
    }
    if count == 0 || count > 30 {
        bail!("Invalid archive path depth");
    }
    Ok(target)
}

fn extract_archive(source: &Path, root: &Path, archive_type: &str) -> Result<()> {
    let mut total = 0u64;
    if archive_type == "zip" {
        let mut archive = zip::ZipArchive::new(fs::File::open(source)?)?;
        if archive.len() > MAX_FILES {
            bail!("Too many files in runtime package");
        }
        for index in 0..archive.len() {
            let mut file = archive.by_index(index)?;
            if file.is_dir() && matches!(file.name(), "." | "./") {
                continue;
            }
            if file
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                bail!("Runtime archives may not contain symbolic links");
            }
            let target = safe_path(root, file.name())?;
            if file.is_dir() {
                fs::create_dir_all(target)?;
                continue;
            }
            total = total
                .checked_add(file.size())
                .context("Archive size overflow")?;
            if total > MAX_EXPANDED_BYTES {
                bail!("Expanded runtime package is too large");
            }
            let size = file.size();
            copy_entry(&mut file, &target, size)?;
        }
    } else if archive_type == "tar.xz" {
        let decoder = xz2::read::XzDecoder::new(fs::File::open(source)?);
        let mut archive = tar::Archive::new(decoder);
        for (index, entry) in archive.entries()?.enumerate() {
            if index >= MAX_FILES {
                bail!("Too many files in runtime package");
            }
            let mut file = entry?;
            let kind = file.header().entry_type();
            if !kind.is_dir() && !kind.is_file() {
                bail!("Runtime archives may only contain regular files and directories");
            }
            let path = file.path()?.into_owned();
            if kind.is_dir() && path == Path::new(".") {
                continue;
            }
            let target = safe_path(root, path.to_str().context("Non-UTF-8 archive path")?)?;
            if kind.is_dir() {
                fs::create_dir_all(target)?;
                continue;
            }
            let size = file.header().size()?;
            total = total.checked_add(size).context("Archive size overflow")?;
            if total > MAX_EXPANDED_BYTES {
                bail!("Expanded runtime package is too large");
            }
            copy_entry(&mut file, &target, size)?;
        }
    } else {
        bail!("Unsupported archive format");
    }
    Ok(())
}

fn copy_entry(reader: &mut impl Read, target: &Path, limit: u64) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(target)?;
    let bytes = io::copy(&mut reader.take(limit + 1), &mut file)?;
    if bytes != limit {
        bail!("Runtime archive entry did not match its advertised size");
    }
    file.flush()?;
    Ok(())
}

fn find_executable(root: &Path, relative: Option<&str>, name: &str) -> Result<PathBuf> {
    if let Some(relative) = relative {
        let path = safe_path(root, relative)?;
        if !path.is_file() {
            bail!("The signed package's executable is missing");
        }
        return Ok(path);
    }
    let mut directories = vec![root.to_path_buf()];
    let mut matches = Vec::new();
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                directories.push(entry.path());
            } else if entry.file_name() == name {
                matches.push(entry.path());
            }
        }
    }
    if matches.len() != 1 {
        bail!("The runtime must contain exactly one {name}, or specify its executable path");
    }
    Ok(matches.remove(0))
}

fn executable_name(name: &str) -> &'static str {
    match (name, cfg!(windows)) {
        ("ferris-worker", true) => "ferris-worker.exe",
        ("ferris-worker", false) => "ferris-worker",
        ("ffmpeg", true) => "ffmpeg.exe",
        ("ffmpeg", false) => "ffmpeg",
        (_, true) => "ffprobe.exe",
        (_, false) => "ffprobe",
    }
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let _ = path;
    Ok(())
}

pub async fn detect_ffmpeg() -> Option<(PathBuf, PathBuf)> {
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let ffmpeg = directory.join(executable_name("ffmpeg"));
        let ffprobe = directory.join(executable_name("ffprobe"));
        if ffmpeg.is_file() && ffprobe.is_file() && check_ffmpeg(&ffmpeg, &ffprobe).await.is_ok() {
            return Some((ffmpeg, ffprobe));
        }
    }
    None
}

pub async fn check_ffmpeg(ffmpeg: &Path, ffprobe: &Path) -> Result<()> {
    let version = command_output(ffprobe, &["-version"]).await?;
    if !version.contains("ffprobe version") {
        bail!("The ffprobe installation is invalid");
    }
    let encoders = command_output(ffmpeg, &["-hide_banner", "-encoders"]).await?;
    if !encoders.split_whitespace().any(|word| word == "libx264") {
        bail!("FFmpeg needs the libx264 encoder");
    }
    let filters = command_output(ffmpeg, &["-hide_banner", "-filters"]).await?;
    if !filters.split_whitespace().any(|word| word == "drawtext") {
        bail!("FFmpeg needs the drawtext filter");
    }
    Ok(())
}

async fn command_output(binary: &Path, args: &[&str]) -> Result<String> {
    let mut command = tokio::process::Command::new(binary);
    command.args(args).kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let output = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .context("The runtime capability check timed out")??;
    if !output.status.success() {
        bail!("The runtime capability check failed");
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(text)
}

async fn check_scene_worker(renderer: &Path, directory: &Path) -> Result<()> {
    let mut command = tokio::process::Command::new(renderer);
    command
        .arg("--capabilities")
        .current_dir(directory)
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let output = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .context("The community renderer capability check timed out")??;
    if !output.status.success() || output.stdout.len() > 16 * 1024 {
        bail!("The runtime package does not contain a compatible Ferris scene worker");
    }
    let capabilities: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("The worker returned invalid capability metadata")?;
    if capabilities
        .get("runtime_kind")
        .and_then(|value| value.as_str())
        != Some(crate::model::WORKER_RUNTIME_KIND)
        || capabilities
            .get("scene_format")
            .and_then(|value| value.as_str())
            != Some(crate::model::SCENE_FORMAT)
        || capabilities
            .get("protocol_version")
            .and_then(|value| value.as_u64())
            != Some(1)
    {
        bail!("The runtime package uses an unsupported scene worker protocol");
    }
    Ok(())
}

pub fn extract_scene(
    source: &Path,
    root: &Path,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<()> {
    const LIMIT: u64 = 2 * 1024 * 1024 * 1024;
    let mut archive = zip::ZipArchive::new(fs::File::open(source)?)?;
    if archive.len() > 8192 {
        bail!("The scene package contains too many entries");
    }
    fs::create_dir(root)?;
    storage::private_directory(root)?;
    let mut expanded = 0u64;
    for index in 0..archive.len() {
        if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            bail!("Scene preparation cancelled");
        }
        let mut file = archive.by_index(index)?;
        if file.unix_mode().is_some_and(|mode| {
            mode & 0o170000 != 0 && !matches!(mode & 0o170000, 0o040000 | 0o100000)
        }) {
            bail!("Scene packages may only contain regular media files and directories");
        }
        if file.is_dir() && matches!(file.name(), "." | "./") {
            continue;
        }
        let path = safe_path(root, file.name())?;
        if file.is_dir() {
            fs::create_dir_all(path)?;
            continue;
        }
        let relative = path.strip_prefix(root)?;
        let name = relative.to_string_lossy().replace('\\', "/");
        let is_manifest = name == "manifest.json";
        let is_commands = name == "commands.fscene.gz";
        let extension = path
            .extension()
            .and_then(|part| part.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let media = name.starts_with("media/")
            && matches!(
                extension.as_str(),
                "png"
                    | "jpg"
                    | "jpeg"
                    | "gif"
                    | "bmp"
                    | "webp"
                    | "apng"
                    | "mp4"
                    | "mkv"
                    | "webm"
                    | "avi"
                    | "mov"
                    | "mp3"
                    | "ogg"
                    | "flac"
                    | "wav"
                    | "m4a"
                    | "aac"
                    | "ttf"
                    | "otf"
                    | "ttc"
            );
        if !is_manifest && !is_commands && !media {
            bail!("The scene package contains an unsupported file type");
        }
        let size = file.size();
        if is_manifest && size > 1024 * 1024 {
            bail!("The scene manifest is too large");
        }
        expanded = expanded.checked_add(size).context("Scene size overflow")?;
        if expanded > LIMIT {
            bail!("The expanded scene package exceeds 2 GB");
        }
        copy_scene_entry(&mut file, &path, size, cancelled)?;
    }
    if !root.join("manifest.json").is_file() || !root.join("commands.fscene.gz").is_file() {
        bail!("The scene package is missing its manifest or drawing commands");
    }
    Ok(())
}

fn copy_scene_entry(
    reader: &mut impl Read,
    target: &Path,
    expected: u64,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let mut buffer = [0u8; 64 * 1024];
    let mut copied = 0u64;
    loop {
        if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            bail!("Scene preparation cancelled");
        }
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        copied = copied
            .checked_add(count as u64)
            .context("Scene entry size overflow")?;
        if copied > expected {
            bail!("Scene entry exceeded its advertised size");
        }
        output.write_all(&buffer[..count])?;
    }
    if copied != expected {
        bail!("Scene entry did not match its advertised size");
    }
    Ok(())
}
