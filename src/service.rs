use crate::{
    api::Api,
    lease::SceneLease,
    model::*,
    runtime::{self, InstalledRuntime, ReleaseManifest},
    storage::{self, Storage},
};
use anyhow::{Context, Result, bail};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    sync::{mpsc, watch},
    time::Instant,
};

#[derive(Clone)]
pub struct Controller {
    pub state: Arc<Mutex<AppState>>,
    pub commands: mpsc::UnboundedSender<Command>,
}

impl Controller {
    pub fn spawn() -> Self {
        let (commands, receiver) = mpsc::unbounded_channel();
        let state = Arc::new(Mutex::new(AppState::default()));
        if let Ok(storage) = Storage::open() {
            let mut initial = state.lock().unwrap_or_else(|p| p.into_inner());
            initial.settings = storage.settings();
            initial.history = storage.history();
            if let Some(runtime) = runtime::installed(&storage) {
                initial.runtime = runtime.info();
            }
        }
        let worker_state = state.clone();
        std::thread::Builder::new()
            .name("Ferris coordinator".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .context("Could not start the Ferris background runtime")
                    .and_then(|runtime| {
                        runtime.block_on(Coordinator::run(worker_state.clone(), receiver))
                    });
                if let Err(error) = result {
                    if let Ok(mut state) = worker_state.lock() {
                        state.connection = ConnectionState::Error;
                        state.message = storage::redact(&format!("{error:#}"));
                    }
                }
            })
            .expect("Could not start the Ferris coordinator thread");
        Self { state, commands }
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn snapshot(&self) -> AppState {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

struct RunningJob {
    lease: String,
    progress: u8,
    cancel: watch::Sender<bool>,
}

enum Event {
    SignedIn(u64, Result<crate::api::TokenResponse>),
    Heartbeat(u64, Result<SlotsResponse>),
    Progress(String, u8, String),
    JobFinished(String, Result<()>),
    Log(String, String),
    Manifest(Result<ReleaseManifest>),
    RuntimeInstalled(Result<InstalledRuntime>),
    ClientDownloaded(Result<PathBuf>),
}

struct Coordinator {
    state: Arc<Mutex<AppState>>,
    storage: Storage,
    api: Api,
    token: Option<String>,
    settings: Settings,
    runtime: Option<InstalledRuntime>,
    manifest: Option<ReleaseManifest>,
    active: HashMap<String, RunningJob>,
    events: mpsc::UnboundedSender<Event>,
    sign_in_cancel: Option<watch::Sender<bool>>,
    sign_in_generation: u64,
    connected: bool,
    accepting: bool,
    next_heartbeat: Instant,
    failures: u32,
    runtime_busy: bool,
    heartbeat_pending: bool,
    connection_epoch: u64,
    next_cleanup: Instant,
}

impl Coordinator {
    async fn run(
        state: Arc<Mutex<AppState>>,
        mut commands: mpsc::UnboundedReceiver<Command>,
    ) -> Result<()> {
        let storage = Storage::open()?;
        let instance_lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(storage.data.join("instance.lock"))?;
        instance_lock
            .try_lock()
            .context("Ferris Client is already running. Open its existing window or tray icon.")?;
        let _instance_lock = instance_lock;
        clean_abandoned_jobs(&storage);
        let settings = storage.settings();
        let api = Api::new(&settings.server_url)?;
        let runtime = runtime::installed(&storage);
        let (events, mut receiver) = mpsc::unbounded_channel();
        {
            let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
            state.settings = settings.clone();
            state.history = storage.history();
            if let Some(runtime) = &runtime {
                state.runtime = runtime.info();
            }
        }
        let mut coordinator = Self {
            state,
            storage,
            api,
            token: None,
            settings,
            runtime,
            manifest: None,
            active: HashMap::new(),
            events,
            sign_in_cancel: None,
            sign_in_generation: 0,
            connected: false,
            accepting: false,
            next_heartbeat: Instant::now(),
            failures: 0,
            runtime_busy: false,
            heartbeat_pending: false,
            connection_epoch: 0,
            next_cleanup: Instant::now() + Duration::from_secs(60),
        };
        coordinator.restore_sign_in().await;
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                command = commands.recv() => {
                    let Some(command) = command else {
                        coordinator.cancel_jobs();
                        break;
                    };
                    if let Err(error) = coordinator.command(command).await {
                        coordinator.error(&error);
                    }
                }
                Some(event) = receiver.recv() => coordinator.event(event).await,
                _ = tick.tick() => {
                    if coordinator.connected && !coordinator.heartbeat_pending && Instant::now() >= coordinator.next_heartbeat {
                        coordinator.heartbeat();
                    }
                    if Instant::now() >= coordinator.next_cleanup {
                        clean_abandoned_jobs(&coordinator.storage);
                        coordinator.next_cleanup = Instant::now() + Duration::from_secs(60);
                    }
                }
            }
        }
        // Child tasks own kill-on-drop processes. Give cancellation a short
        // window to terminate process trees and send failure reports.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !coordinator.active.is_empty() && Instant::now() < deadline {
            if let Ok(Some(event)) =
                tokio::time::timeout(Duration::from_secs(1), receiver.recv()).await
            {
                coordinator.event(event).await;
            }
        }
        Ok(())
    }

    fn update(&self, update: impl FnOnce(&mut AppState)) {
        update(&mut self.state.lock().unwrap_or_else(|p| p.into_inner()));
    }

    fn log(&self, level: &str, message: &str) {
        let message = storage::redact(message);
        let entry = LogEntry {
            timestamp: chrono::Utc::now().to_rfc3339(),
            level: level.into(),
            message,
        };
        let _ = self.storage.append_log(&format!(
            "{} [{}] {}",
            entry.timestamp, level, entry.message
        ));
        self.update(|state| {
            state.logs.push(entry);
            if state.logs.len() > 500 {
                state.logs.drain(..state.logs.len() - 500);
            }
        });
    }

    fn error(&self, error: &anyhow::Error) {
        let message = storage::redact(&format!("{error:#}"));
        self.log("error", &message);
        self.update(|state| {
            state.message = message;
        });
    }

    async fn restore_sign_in(&mut self) {
        match self.storage.load_token(&self.settings.server_url) {
            Ok(Some(token)) => match self.api.profile(&token).await {
                Ok(profile) => {
                    self.token = Some(token);
                    self.update(|state| {
                        state.profile = Some(profile);
                        state.connection = ConnectionState::Ready;
                        state.message = "Signed in. Start contributing when you are ready.".into();
                    });
                }
                Err(error) => {
                    self.error(&error);
                    if is_unauthorized(&error) {
                        let _ = self.storage.delete_token(&self.settings.server_url);
                    }
                }
            },
            Ok(None) => {}
            Err(error) => self.error(&error),
        }
    }

    async fn command(&mut self, command: Command) -> Result<()> {
        match command {
            Command::SignIn => self.sign_in().await?,
            Command::CancelSignIn => {
                self.sign_in_generation = self.sign_in_generation.wrapping_add(1);
                if let Some(cancel) = self.sign_in_cancel.take() {
                    let _ = cancel.send(true);
                }
                self.update(|state| {
                    state.device_auth = None;
                    state.connection = ConnectionState::Offline;
                });
            }
            Command::Connect => {
                if self.token.is_none() {
                    bail!("Sign in with osu! first");
                }
                if self.runtime.is_none() {
                    bail!("Install the signed render tools before connecting");
                }
                if platform_name() == "unsupported" {
                    bail!("This operating system or CPU is not supported by this build");
                }
                self.connected = true;
                self.accepting = true;
                self.connection_epoch = self.connection_epoch.wrapping_add(1);
                self.heartbeat_pending = false;
                self.failures = 0;
                self.next_heartbeat = Instant::now();
                self.update(|state| {
                    state.connection = ConnectionState::Connected;
                    state.message = "Connected. Ferris can assign work to this computer.".into();
                });
                self.log(
                    "info",
                    "Started accepting render work after your confirmation.",
                );
            }
            Command::Drain => {
                self.accepting = false;
                self.next_heartbeat = Instant::now();
                self.update(|state| {
                    state.connection = ConnectionState::Draining;
                    state.message = "Finishing current renders. No new work will start.".into();
                });
            }
            Command::CancelJobs => {
                self.accepting = false;
                self.cancel_jobs();
                self.next_heartbeat = Instant::now();
                self.update(|state| {
                    state.connection = ConnectionState::Draining;
                    state.message = "Cancelling this computer's renders.".into();
                });
            }
            Command::SignOut => {
                self.connection_epoch = self.connection_epoch.wrapping_add(1);
                self.heartbeat_pending = false;
                self.sign_in_generation = self.sign_in_generation.wrapping_add(1);
                self.accepting = false;
                self.connected = false;
                self.cancel_jobs();
                if let Some(cancel) = self.sign_in_cancel.take() {
                    let _ = cancel.send(true);
                }
                if let Some(token) = self.token.take() {
                    let _ = self.api.revoke(&token).await;
                }
                self.storage.delete_token(&self.settings.server_url)?;
                self.update(|state| {
                    state.profile = None;
                    state.device_auth = None;
                    state.connection = ConnectionState::Offline;
                    state.message = "Signed out.".into();
                });
            }
            Command::SaveSettings(mut settings) => {
                if settings.capacity == 0 {
                    settings.capacity = 1;
                }
                let api = Api::new(&settings.server_url)?;
                settings.server_url = api.base.as_str().trim_end_matches('/').to_owned();
                if settings.server_url != self.settings.server_url {
                    if self.connected || !self.active.is_empty() || self.runtime_busy {
                        bail!("Disconnect and finish current work before changing servers");
                    }
                    self.token = None;
                    self.manifest = None;
                    self.sign_in_generation = self.sign_in_generation.wrapping_add(1);
                    if let Some(cancel) = self.sign_in_cancel.take() {
                        let _ = cancel.send(true);
                    }
                    self.api = api;
                    self.update(|state| {
                        state.profile = None;
                        state.connection = ConnectionState::Offline;
                    });
                }
                self.storage.save_settings(&settings)?;
                self.settings = settings.clone();
                self.update(|state| {
                    state.settings = settings;
                    state.message = "Settings saved.".into();
                });
            }
            Command::CheckUpdates => self.check_updates(),
            Command::InstallRuntime => {
                if self.runtime_busy {
                    bail!("A runtime download is already in progress");
                }
                if self.connected || !self.active.is_empty() {
                    bail!("Disconnect before installing a runtime update");
                }
                self.runtime_busy = true;
                self.update(|state| {
                    state.runtime.installing = true;
                    state.runtime.message =
                        "Checking the signed manifest and installing verified tools…".into();
                });
                let api = self.api.clone();
                let storage = self.storage.clone();
                let events = self.events.clone();
                tokio::spawn(async move {
                    let result = async {
                        let manifest = runtime::fetch_manifest(&api).await?;
                        runtime::install(&api, &storage, &manifest).await
                    }
                    .await;
                    let _ = events.send(Event::RuntimeInstalled(result));
                });
            }
            Command::DownloadClientUpdate => {
                let manifest = self
                    .manifest
                    .clone()
                    .context("Check for a signed client update first")?;
                let api = self.api.clone();
                let storage = self.storage.clone();
                let events = self.events.clone();
                tokio::spawn(async move {
                    let result = runtime::download_client(&api, &storage, &manifest).await;
                    let _ = events.send(Event::ClientDownloaded(result));
                });
            }
        }
        Ok(())
    }

    async fn sign_in(&mut self) -> Result<()> {
        if self.sign_in_cancel.is_some() {
            bail!("A sign-in is already in progress");
        }
        if self.token.is_some() {
            bail!("You are already signed in");
        }
        let auth = self.api.begin_sign_in().await?;
        let deadline = Instant::now() + Duration::from_secs(auth.expires_in.min(1800));
        let interval = Duration::from_secs(auth.interval.clamp(5, 30));
        self.update(|state| {
            state.device_auth = Some(auth.clone());
            state.connection = ConnectionState::SigningIn;
            state.message = "Approve the device code in your browser.".into();
        });
        let (cancel, mut cancelled) = watch::channel(false);
        self.sign_in_cancel = Some(cancel);
        self.sign_in_generation = self.sign_in_generation.wrapping_add(1);
        let generation = self.sign_in_generation;
        let api = self.api.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancelled.changed() => return,
                    _ = tokio::time::sleep(interval) => {},
                }
                if Instant::now() >= deadline {
                    let _ = events.send(Event::SignedIn(
                        generation,
                        Err(anyhow::anyhow!(
                            "The sign-in code expired. Start sign-in again."
                        )),
                    ));
                    return;
                }
                let result = tokio::select! {
                    _ = cancelled.changed() => return,
                    result = api.poll_sign_in(&auth.device_code) => result,
                };
                match result {
                    Ok(Some(token)) => {
                        let _ = events.send(Event::SignedIn(generation, Ok(token)));
                        return;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = events.send(Event::SignedIn(generation, Err(error)));
                        return;
                    }
                }
            }
        });
        Ok(())
    }

    fn check_updates(&self) {
        let api = self.api.clone();
        let events = self.events.clone();
        self.update(|state| {
            state.message = "Checking signed release metadata…".into();
        });
        tokio::spawn(async move {
            let result = runtime::fetch_manifest(&api).await;
            let _ = events.send(Event::Manifest(result));
        });
    }

    fn cancel_jobs(&self) {
        for job in self.active.values() {
            let _ = job.cancel.send(true);
        }
    }

    async fn event(&mut self, event: Event) {
        match event {
            Event::SignedIn(generation, result) => {
                if generation != self.sign_in_generation { return; }
                self.sign_in_cancel = None;
                self.update(|state| { state.device_auth = None; });
                match result {
                    Ok(response) => match self.storage.save_token(&self.settings.server_url, &response.access_token) {
                        Ok(()) => {
                            self.token = Some(response.access_token);
                            self.update(|state| { state.profile = Some(response.user); state.connection = ConnectionState::Ready;
                                state.message = "Signed in. This computer will render only after you start contributing.".into(); });
                            self.log("info", "osu! device sign-in completed. Credential saved in the operating system vault.");
                        }
                        Err(error) => {
                            let _ = self.api.revoke(&response.access_token).await;
                            self.update(|state| { state.connection = ConnectionState::Offline; });
                            self.error(&error);
                        }
                    },
                    Err(error) => { self.update(|state| { state.connection = ConnectionState::Offline; }); self.error(&error); }
                }
            }
            Event::Heartbeat(epoch, result) => {
                if epoch != self.connection_epoch { return; }
                self.heartbeat_pending = false;
                if self.connected { self.heartbeat_result(result).await; }
            }
            Event::Progress(id, progress, status) => {
                if let Some(job) = self.active.get_mut(&id) { job.progress = progress; }
                self.update(|state| { if let Some(job) = state.jobs.iter_mut().find(|job| job.id == id) {
                    job.progress = progress; job.status = status;
                } });
            }
            Event::JobFinished(id, result) => {
                self.active.remove(&id);
                let success = result.is_ok();
                if let Err(error) = result { self.error(&error); }
                self.log(if success { "info" } else { "error" }, &format!("Render {id} {}.", if success { "uploaded" } else { "failed or cancelled" }));
                self.update(|state| {
                    state.jobs.retain(|job| job.id != id);
                    state.history.insert(0, HistoryEntry { id, finished_at: chrono::Utc::now().to_rfc3339(), result: if success { "Uploaded" } else { "Failed / cancelled" }.into() });
                    state.history.truncate(200);
                    let _ = self.storage.save_history(&state.history);
                });
                self.next_heartbeat = Instant::now();
            }
            Event::Log(level, message) => self.log(&level, &message),
            Event::Manifest(result) => match result {
                Ok(manifest) => {
                    self.update(|state| { state.update_available = (manifest.version != CLIENT_VERSION && manifest.artifacts.client.is_some()).then(|| manifest.version.clone());
                        state.message = format!("Verified signed release {}.", manifest.version); });
                    self.manifest = Some(manifest);
                }
                Err(error) => self.error(&error),
            },
            Event::RuntimeInstalled(result) => {
                self.runtime_busy = false;
                self.update(|state| { state.runtime.installing = false; });
                match result {
                    Ok(runtime) => {
                        self.update(|state| { state.runtime = runtime.info(); state.message = "Signed render tools installed. You can start contributing.".into(); });
                        self.log("info", "Installed and verified the signed renderer and FFmpeg capabilities.");
                        self.runtime = Some(runtime);
                    }
                    Err(error) => { self.error(&error); self.update(|state| { state.runtime.message = "Installation failed. Previous tools are unchanged.".into(); }); }
                }
            }
            Event::ClientDownloaded(result) => match result {
                Ok(path) => self.update(|state| { state.update_path = Some(path);
                    state.message = "Signed installer downloaded. Open it when you are ready; it will not run automatically.".into(); }),
                Err(error) => self.error(&error),
            },
        }
    }

    fn heartbeat(&mut self) {
        let Some(token) = self.token.clone() else {
            self.connected = false;
            return;
        };
        let request = SlotsRequest {
            scene_format: SCENE_FORMAT.into(),
            capacity: self.settings.capacity,
            accept_jobs: self.accepting,
            client_version: CLIENT_VERSION.into(),
            platform: platform_name().into(),
            specs: machine_specs(
                self.settings.share_machine_info,
                self.settings.render_backend,
            ),
            active_jobs: self
                .active
                .iter()
                .map(|(id, job)| ActiveJob {
                    id: id.clone(),
                    lease_token: job.lease.clone(),
                    progress: job.progress,
                })
                .collect(),
        };
        self.heartbeat_pending = true;
        let epoch = self.connection_epoch;
        let api = self.api.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            let result = api.slots(&token, &request).await;
            let _ = events.send(Event::Heartbeat(epoch, result));
        });
    }

    async fn heartbeat_result(&mut self, result: Result<SlotsResponse>) {
        match result {
            Ok(response) => {
                let Some(node_id) = response
                    .node_id
                    .as_str()
                    .filter(|id| uuid::Uuid::parse_str(id).is_ok())
                    .map(str::to_owned)
                else {
                    self.error(&anyhow::anyhow!("Ferris returned an invalid node identity"));
                    self.next_heartbeat = Instant::now() + Duration::from_secs(30);
                    return;
                };
                self.failures = 0;
                self.next_heartbeat =
                    Instant::now() + Duration::from_secs(response.poll_after.clamp(5, 30));
                for job in response.jobs {
                    if !self.accepting
                        || self.active.len() >= self.settings.capacity as usize
                        || self.active.contains_key(&job.id)
                    {
                        if let Some(token) = self.token.clone() {
                            let api = self.api.clone();
                            tokio::spawn(async move {
                                let _ = api
                                    .progress(
                                        &token,
                                        &job.id,
                                        &job.lease_token,
                                        0,
                                        Some("Node capacity changed before job acceptance"),
                                    )
                                    .await;
                            });
                        }
                        continue;
                    }
                    if let Some(token) = self.token.clone() {
                        self.start_job(job, token, node_id.clone());
                    }
                }
                if !self.accepting && self.active.is_empty() {
                    self.connected = false;
                    self.update(|state| {
                        state.connection = ConnectionState::Ready;
                        state.message = "Disconnected. All current work has finished.".into();
                    });
                }
            }
            Err(error) => {
                self.failures = self.failures.saturating_add(1);
                let delay = (5 * 2u64.saturating_pow(self.failures.min(5))).min(120);
                self.next_heartbeat = Instant::now() + Duration::from_secs(delay);
                if is_unauthorized(&error) || error.to_string().contains("HTTP 403") {
                    self.connected = false;
                    self.accepting = false;
                    self.cancel_jobs();
                    self.token = None;
                    let _ = self.storage.delete_token(&self.settings.server_url);
                    self.update(|state| {
                        state.profile = None;
                        state.connection = ConnectionState::Offline;
                    });
                }
                self.error(&error);
            }
        }
    }

    fn start_job(&mut self, job: RenderJob, token: String, node_id: String) {
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let (cancel, cancelled) = watch::channel(false);
        let id = job.id.clone();
        self.active.insert(
            id.clone(),
            RunningJob {
                lease: job.lease_token.clone(),
                progress: 0,
                cancel,
            },
        );
        self.update(|state| {
            state.jobs.push(JobView {
                id: id.clone(),
                progress: 0,
                status: "Downloading".into(),
            });
        });
        let api = self.api.clone();
        let render_backend = self.settings.render_backend;
        let storage = self.storage.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            let result = run_job(
                &api,
                &storage,
                &runtime,
                &token,
                &job,
                &node_id,
                render_backend,
                cancelled,
                &events,
            )
            .await;
            if let Err(error) = &result {
                let message = storage::redact(&format!("{error:#}"));
                let _ = api
                    .progress(&token, &id, &job.lease_token, 0, Some(&message))
                    .await;
            }
            let _ = events.send(Event::JobFinished(id, result));
        });
    }
}

fn is_unauthorized(error: &anyhow::Error) -> bool {
    error.to_string().contains("HTTP 401")
}

fn machine_specs(share_names: bool, render_backend: RenderBackend) -> MachineSpecs {
    let system = sysinfo::System::new_all();
    MachineSpecs {
        render_backend,
        cpu_name: system
            .cpus()
            .first()
            .map(|cpu| cpu.brand().to_owned())
            .unwrap_or_else(|| "Unknown CPU".into()),
        cpu_threads: system.cpus().len().max(1),
        memory_bytes: system.total_memory(),
        gpu_name: None,
        pc_username: share_names.then(|| {
            std::env::var("USERNAME")
                .or_else(|_| std::env::var("USER"))
                .unwrap_or_else(|_| "Unknown".into())
        }),
        hostname: if share_names {
            sysinfo::System::host_name()
        } else {
            None
        },
    }
}

async fn run_job(
    api: &Api,
    storage: &Storage,
    runtime: &InstalledRuntime,
    token: &str,
    job: &RenderJob,
    node_id: &str,
    render_backend: RenderBackend,
    mut cancelled: watch::Receiver<bool>,
    events: &mpsc::UnboundedSender<Event>,
) -> Result<()> {
    let id = uuid::Uuid::parse_str(&job.id).context("Invalid render job ID")?;
    if job.scene_format != SCENE_FORMAT {
        bail!("This job uses an unsupported scene format");
    }
    if job.assets.len() != 1 || job.assets[0].kind != "scene" {
        bail!("A scene job must contain exactly one prepared scene package");
    }
    let expected_permit = api.endpoint(&format!("jobs/{id}/permit"))?;
    if api.same_origin_url(&job.permit_url)? != expected_permit {
        bail!("Unexpected render permit endpoint");
    }
    let expected_upload = api.endpoint(&format!("jobs/{id}/complete"))?;
    if api.same_origin_url(&job.upload_url)? != expected_upload {
        bail!("Unexpected video upload endpoint");
    }
    let jobs_root = storage.cache.join("jobs");
    tokio::fs::create_dir_all(&jobs_root).await?;
    let directory = jobs_root.join(format!("{id}-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir(&directory).await?;
    storage::private_directory(&directory)?;
    let result = async {
        let asset = &job.assets[0];
        let archive_path = directory.join("scene.zip");
        tokio::select! {
            _ = cancelled.changed() => bail!("Render cancelled by the contributor"),
            result = api.download_asset(token, &asset.url, &archive_path, &asset.sha256, asset.bytes, 300 * 1024 * 1024) => result?,
        }
        let scene_sha256 = asset.sha256.to_ascii_lowercase();
        let authorization = tokio::select! {
            _ = cancelled.changed() => bail!("Render cancelled by the contributor"),
            result = SceneLease::authorize(api, token, &job.id, &job.lease_token, &scene_sha256, node_id) => result?,
        };
        let scene_directory = directory.join("scene");
        let source = archive_path.clone();
        let destination = scene_directory.clone();
        let extraction_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancelled_for_task = extraction_cancel.clone();
        let mut extraction = tokio::task::spawn_blocking(move || runtime::extract_scene(&source, &destination, &cancelled_for_task));
        tokio::select! {
            result = &mut extraction => result.context("The scene extraction task failed")??,
            _ = cancelled.changed() => {
                extraction_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                let _ = extraction.await;
                bail!("Render cancelled by the contributor");
            }
        }
        if *cancelled.borrow() { bail!("Render cancelled by the contributor"); }
        tokio::fs::remove_file(&archive_path).await?;
        let output = directory.join("render.mp4");
        let mut args = vec!["--scene".into(), scene_directory.to_string_lossy().into_owned(),
            "--out".into(), output.to_string_lossy().into_owned(), "--ffmpeg".into(), runtime.ffmpeg.to_string_lossy().into_owned()];
        if render_backend == RenderBackend::Cpu { args.push("--cpu-renderer".into()); }
        let _ = events.send(Event::Progress(job.id.clone(), 0, "Rendering".into()));
        tokio::select! {
            _ = cancelled.changed() => bail!("Render cancelled by the contributor"),
            _ = api.progress(token, &job.id, &job.lease_token, 0, None) => {},
        }
        render_process(runtime, &args, &directory, &job.id, authorization, cancelled.clone(), events).await?;
        if *cancelled.borrow() { bail!("Render cancelled by the contributor"); }
        let _ = events.send(Event::Progress(job.id.clone(), 99, "Uploading".into()));
        tokio::select! {
            _ = cancelled.changed() => bail!("Render cancelled by the contributor"),
            result = api.complete(token, &job.upload_url, &job.lease_token, &output) => result?,
        }
        Ok(())
    }.await;
    if directory.starts_with(&jobs_root) {
        let _ = tokio::fs::remove_dir_all(&directory).await;
    }
    result
}

async fn render_process(
    runtime: &InstalledRuntime,
    args: &[String],
    job_directory: &Path,
    id: &str,
    mut authorization: SceneLease,
    mut cancelled: watch::Receiver<bool>,
    events: &mpsc::UnboundedSender<Event>,
) -> Result<()> {
    if *cancelled.borrow() {
        bail!("Render cancelled by the contributor");
    }
    if !authorization.valid() {
        bail!("The signed scene lease expired before rendering could start");
    }
    let ffmpeg_bin = runtime.ffmpeg.parent().context("Invalid FFmpeg path")?;
    let mut paths = vec![ffmpeg_bin.to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let temp = job_directory.join("tmp");
    tokio::fs::create_dir(&temp).await?;
    let mut command = tokio::process::Command::new(&runtime.renderer);
    command
        .args(args)
        .current_dir(&runtime.root)
        .env("PATH", std::env::join_paths(paths)?)
        .env("TMPDIR", &temp)
        .env("TMP", &temp)
        .env("TEMP", &temp)
        .env_remove("OSU_CLIENT_SECRET")
        .env_remove("OSU_CLIENT_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let mut child = command
        .spawn()
        .context("Could not start the signed Ferris scene worker")?;
    let pid = child.id().context("The renderer has no process ID")?;
    #[cfg(windows)]
    let process_tree = WindowsProcessTree::attach(pid)?;
    let stdout = child
        .stdout
        .take()
        .context("Missing renderer output stream")?;
    let stderr = child
        .stderr
        .take()
        .context("Missing renderer error stream")?;
    let mut stdout_task = tokio::spawn(read_render_output(stdout, id.to_owned(), events.clone()));
    let mut stderr_task = tokio::spawn(read_render_output(stderr, id.to_owned(), events.clone()));
    let result = tokio::select! {
        lease = authorization.maintain() => {
            terminate_tree(pid).await;
            let _ = child.kill().await;
            let _ = child.wait().await;
            lease.and_then(|_| Err(anyhow::anyhow!("The signed scene lease ended unexpectedly")))
        }
        _ = cancelled.changed() => {
            terminate_tree(pid).await;
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(anyhow::anyhow!("Render cancelled by the contributor"))
        }
        _ = tokio::time::sleep(Duration::from_secs(4 * 60 * 60)) => {
            terminate_tree(pid).await;
            let _ = child.kill().await;
            Err(anyhow::anyhow!("The renderer exceeded the four-hour job limit"))
        }
        status = child.wait() => {
            match status {
                Ok(status) if status.success() => Ok(()),
                Ok(status) => Err(anyhow::anyhow!("Ferris worker exited unsuccessfully (code {:?}); see the local render logs", status.code())),
                Err(error) => Err(error).context("Could not wait for the renderer process"),
            }
        }
    };
    // A crashed renderer may leave FFmpeg alive with inherited stdout handles.
    if result.is_err() {
        terminate_tree(pid).await;
    }
    #[cfg(windows)]
    drop(process_tree);
    if tokio::time::timeout(Duration::from_secs(3), &mut stdout_task)
        .await
        .is_err()
    {
        stdout_task.abort();
    }
    if tokio::time::timeout(Duration::from_secs(3), &mut stderr_task)
        .await
        .is_err()
    {
        stderr_task.abort();
    }
    if tokio::fs::metadata(job_directory.join("render.mp4"))
        .await
        .is_ok_and(|metadata| metadata.len() > 300 * 1024 * 1024)
    {
        // The worker can enforce the same output limit before its final exit.
        // Preserve the fallback marker even when that exit is unsuccessful.
        bail!("result_too_large: rendered video exceeds the 300 MB node upload limit");
    }
    result
}

async fn read_render_output(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    id: String,
    events: mpsc::UnboundedSender<Event>,
) {
    let mut buffer = [0u8; 4096];
    let mut line = Vec::new();
    let mut last_progress = 0;
    let mut logged_lines = 0usize;
    loop {
        let count = match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        for byte in &buffer[..count] {
            if *byte == b'\n' || *byte == b'\r' {
                let text = String::from_utf8_lossy(&line);
                if let Some(progress) = parse_progress(&text) {
                    if progress != last_progress {
                        last_progress = progress;
                        let _ =
                            events.send(Event::Progress(id.clone(), progress, "Rendering".into()));
                    }
                } else if !text.trim().is_empty() && logged_lines < 2_000 {
                    logged_lines += 1;
                    let _ = events.send(Event::Log(
                        "renderer".into(),
                        format!(
                            "{id}: {}",
                            storage::redact(&text)
                                .chars()
                                .take(1000)
                                .collect::<String>()
                        ),
                    ));
                }
                line.clear();
            } else if line.len() < 8192 {
                line.push(*byte);
            }
        }
    }
}

fn parse_progress(text: &str) -> Option<u8> {
    let text = text.split("Rendering frame ").nth(1)?;
    let (frame, total) = text.split_once('/')?;
    let frame: u64 = frame.trim().parse().ok()?;
    let total: u64 = total
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    if total == 0 {
        return None;
    }
    Some(((frame.saturating_mul(98) / total).min(98)) as u8)
}

async fn terminate_tree(pid: u32) {
    #[cfg(windows)]
    {
        let taskkill = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\Windows"))
            .join("System32\\taskkill.exe");
        let mut command = tokio::process::Command::new(taskkill);
        command
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(0x08000000)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = tokio::time::timeout(Duration::from_secs(5), command.status()).await;
    }
    #[cfg(unix)]
    {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

// Keep FFmpeg and other renderer descendants attached to this client-owned
// Windows job. Closing the handle also works when the worker crashes before FFmpeg.
#[cfg(windows)]
struct WindowsProcessTree(usize);

#[cfg(windows)]
impl WindowsProcessTree {
    fn attach(pid: u32) -> Result<Self> {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::{
                JobObjects::{
                    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                    SetInformationJobObject,
                },
                Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
            },
        };
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(std::io::Error::last_os_error())
                    .context("Could not create the renderer process group");
            }
            let job = Self(handle as usize);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            ) == 0
            {
                return Err(std::io::Error::last_os_error())
                    .context("Could not protect the renderer process group");
            }
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                return Err(std::io::Error::last_os_error())
                    .context("Could not open the renderer process");
            }
            let assigned = AssignProcessToJobObject(handle, process);
            let error = std::io::Error::last_os_error();
            CloseHandle(process);
            if assigned == 0 {
                return Err(error)
                    .context("Could not attach the renderer to the client process group");
            }
            Ok(job)
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsProcessTree {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0 as *mut std::ffi::c_void);
        }
    }
}

fn clean_abandoned_jobs(storage: &Storage) {
    let root = storage.cache.join("jobs");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        if !path.starts_with(&root) {
            continue;
        }
        let stale = metadata
            .modified()
            .ok()
            .and_then(|time| time.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(24 * 60 * 60));
        if stale {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}
