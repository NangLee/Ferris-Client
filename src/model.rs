use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const SCENE_FORMAT: &str = "ferris-scene-v1";
pub const WORKER_RUNTIME_KIND: &str = "ferris-scene-worker-v1";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderBackend {
    #[default]
    Gpu,
    Cpu,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub server_url: String,
    pub capacity: u16,
    pub autostart: bool,
    pub minimize_to_tray: bool,
    pub start_minimized: bool,
    pub share_machine_info: bool,
    pub render_backend: RenderBackend,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            server_url: "https://ferrisfun.win".into(),
            capacity: 10,
            autostart: false,
            minimize_to_tray: true,
            start_minimized: false,
            share_machine_info: false,
            render_backend: RenderBackend::Gpu,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub id: serde_json::Value,
    pub osu_id: serde_json::Value,
    pub username: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceAuth {
    #[serde(skip_serializing)]
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionState {
    #[default]
    Offline,
    SigningIn,
    Ready,
    Connected,
    Draining,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobView {
    pub id: String,
    pub progress: u8,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: String,
    pub finished_at: String,
    pub result: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub installed: bool,
    pub installing: bool,
    pub version: Option<String>,
    pub renderer_path: Option<PathBuf>,
    pub ffmpeg_path: Option<PathBuf>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppState {
    pub settings: Settings,
    pub profile: Option<Profile>,
    pub device_auth: Option<DeviceAuth>,
    pub connection: ConnectionState,
    pub message: String,
    pub jobs: Vec<JobView>,
    pub logs: Vec<LogEntry>,
    pub history: Vec<HistoryEntry>,
    pub runtime: RuntimeInfo,
    pub update_path: Option<PathBuf>,
    pub update_available: Option<String>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            profile: None,
            device_auth: None,
            connection: ConnectionState::Offline,
            message: "Sign in to install the render tools".into(),
            jobs: Vec::new(),
            logs: Vec::new(),
            history: Vec::new(),
            runtime: RuntimeInfo::default(),
            update_path: None,
            update_available: None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Command {
    SignIn,
    CancelSignIn,
    Connect,
    Drain,
    CancelJobs,
    SignOut,
    SaveSettings(Settings),
    InstallRuntime,
    CheckUpdates,
    DownloadClientUpdate,
}

#[derive(Clone, Debug, Serialize)]
pub struct MachineSpecs {
    pub cpu_name: String,
    pub cpu_threads: usize,
    pub memory_bytes: u64,
    pub gpu_name: Option<String>,
    pub pc_username: Option<String>,
    pub hostname: Option<String>,
    pub render_backend: RenderBackend,
}

#[derive(Clone, Debug, Serialize)]
pub struct ActiveJob {
    pub id: String,
    pub lease_token: String,
    pub progress: u8,
}

#[derive(Clone, Debug, Serialize)]
pub struct SlotsRequest {
    pub scene_format: String,
    pub capacity: u16,
    pub accept_jobs: bool,
    pub client_version: String,
    pub platform: String,
    pub specs: MachineSpecs,
    pub active_jobs: Vec<ActiveJob>,
}

#[derive(Debug, Deserialize)]
pub struct SlotsResponse {
    pub node_id: serde_json::Value,
    #[serde(default = "poll_interval")]
    pub poll_after: u64,
    #[serde(default)]
    pub jobs: Vec<RenderJob>,
    pub manifest_url: Option<String>,
}

fn poll_interval() -> u64 {
    5
}

#[derive(Clone, Debug, Deserialize)]
pub struct RenderJob {
    pub id: String,
    pub lease_token: String,
    pub lease_expires_at: serde_json::Value,
    pub scene_format: String,
    pub assets: Vec<JobAsset>,
    pub permit_url: String,
    pub upload_url: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct JobAsset {
    pub kind: String,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
}

pub fn platform_name() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "windows-x86_64",
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        _ => "unsupported",
    }
}
