#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod platform;

use eframe::egui::{self, Color32, RichText, Vec2};
use ferris_client::{
    AppState, CLIENT_VERSION, Command, ConnectionState, Controller, RenderBackend, Settings,
};
use platform::{SystemTray, TrayAction};
use std::time::Duration;

const BACKGROUND: Color32 = Color32::from_rgb(32, 32, 38);
const PINK: Color32 = Color32::from_rgb(255, 107, 139);
const BLUE: Color32 = Color32::from_rgb(110, 208, 255);
const WHITE: Color32 = Color32::WHITE;

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Node,
    Settings,
    Activity,
    About,
}

struct FerrisApp {
    controller: Controller,
    page: Page,
    settings: Settings,
    settings_loaded: bool,
    consent: bool,
    tray: Option<SystemTray>,
    message: String,
    exit_requested: bool,
    exit_confirm: bool,
    log_filter: String,
    opened_auth_code: Option<String>,
}

impl FerrisApp {
    fn new(context: &eframe::CreationContext<'_>) -> Self {
        configure_theme(&context.egui_ctx);
        let controller = Controller::spawn();
        let tray = SystemTray::new().ok();
        let settings = controller
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .settings
            .clone();
        if tray.is_some()
            && (settings.start_minimized
                || std::env::args().any(|argument| argument == "--minimized"))
        {
            context
                .egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        Self {
            controller,
            page: Page::Node,
            settings,
            settings_loaded: false,
            consent: false,
            tray,
            message: String::new(),
            exit_requested: false,
            exit_confirm: false,
            log_filter: String::new(),
            opened_auth_code: None,
        }
    }

    fn send(&mut self, command: Command) {
        if self.controller.commands.send(command).is_err() {
            self.message = "The worker service stopped. Restart Ferris to reconnect.".into();
        }
    }

    fn sidebar(&mut self, context: &egui::Context, state: &AppState) {
        egui::SidePanel::left("navigation")
            .exact_width(190.0)
            .resizable(false)
            .show(context, |ui| {
                ui.add_space(26.0);
                let (area, _) =
                    ui.allocate_exact_size(Vec2::new(145.0, 56.0), egui::Sense::hover());
                ui.painter()
                    .circle_filled(area.left_top() + Vec2::new(25.0, 27.0), 19.0, PINK);
                ui.painter()
                    .circle_filled(area.left_top() + Vec2::new(51.0, 27.0), 15.0, BLUE);
                ui.label(RichText::new("ferris").size(32.0).strong().color(WHITE));
                ui.label(RichText::new("COMMUNITY CLIENT").size(10.0).color(BLUE));
                ui.add_space(38.0);
                for (page, label) in [
                    (Page::Node, "Render node"),
                    (Page::Settings, "Settings"),
                    (Page::Activity, "Activity"),
                    (Page::About, "About"),
                ] {
                    let selected = self.page == page;
                    if ui
                        .add_sized(
                            [165.0, 42.0],
                            egui::Button::new(RichText::new(label).color(if selected {
                                PINK
                            } else {
                                WHITE
                            }))
                            .selected(selected),
                        )
                        .clicked()
                    {
                        self.page = page;
                    }
                    ui.add_space(7.0);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.label(
                        RichText::new(format!("v{CLIENT_VERSION}"))
                            .small()
                            .color(BLUE),
                    );
                    ui.label(
                        state
                            .profile
                            .as_ref()
                            .map(|profile| format!("osu! / {}", profile.username))
                            .unwrap_or_else(|| "Not signed in".into()),
                    );
                    ui.add_space(10.0);
                });
            });
    }

    fn node_page(&mut self, ui: &mut egui::Ui, state: &AppState) {
        heading(
            ui,
            "Main",
            "Help",
        );
        ui.horizontal(|ui| {
            metric(ui, "STATUS", connection_label(&state.connection), PINK);
            metric(ui, "RENDERING", &state.jobs.len().to_string(), BLUE);
            metric(
                ui,
                "SLOT LIMIT",
                &state.settings.capacity.to_string(),
                WHITE,
            );
        });
        ui.add_space(24.0);
        ui.label(&state.message);
        ui.add_space(18.0);

        if state.profile.is_none() {
            if let Some(auth) = &state.device_auth {
                ui.label("Approve this device in your browser using your existing Ferris account.");
                ui.label(
                    RichText::new(&auth.user_code)
                        .size(27.0)
                        .strong()
                        .color(BLUE),
                );
                ui.horizontal(|ui| {
                    if ui.button("Open sign-in page").clicked() {
                        self.open(&auth.verification_uri);
                    }
                    if ui.button("Cancel").clicked() {
                        self.send(Command::CancelSignIn);
                    }
                });
            } else if ui
                .add_enabled(
                    state.connection != ConnectionState::SigningIn,
                    primary("Sign in with osu!"),
                )
                .clicked()
            {
                self.send(Command::SignIn);
            }
        } else {
            ui.label(format!(
                "Signed in as {}",
                state.profile.as_ref().unwrap().username
            ));
        }
        ui.add_space(18.0);
        if !state.runtime.installed {
            ui.label("Install the verified render tools before connecting.");
            if ui
                .add_enabled(
                    !state.runtime.installing,
                    primary(if state.runtime.installing {
                        "Installing render tools…"
                    } else {
                        "Install worker and FFmpeg"
                    }),
                )
                .clicked()
            {
                self.send(Command::InstallRuntime);
            }
        } else {
            ui.label(
                RichText::new(format!(
                    "Render tools ready / {}",
                    state
                        .runtime
                        .version
                        .as_deref()
                        .unwrap_or("managed runtime")
                ))
                .color(BLUE),
            );
        }
        if !state.runtime.message.is_empty() {
            ui.label(&state.runtime.message);
        }
        ui.add_space(22.0);
        ui.label("Connecting shares your approximate location and pc hardware details to the server");
        ui.checkbox(
            &mut self.consent,
            "I agree to share these details and contribute computing power.",
        );
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if matches!(
                state.connection,
                ConnectionState::Connected | ConnectionState::Draining
            ) {
                if ui
                    .add_enabled(
                        state.connection != ConnectionState::Draining,
                        primary("Finish renders and pause"),
                    )
                    .clicked()
                {
                    self.send(Command::Drain);
                }
            } else if ui
                .add_enabled(
                    self.consent
                        && state.profile.is_some()
                        && state.runtime.installed
                        && !state.runtime.installing,
                    primary("Start contributing"),
                )
                .clicked()
            {
                self.send(Command::Connect);
            }
            if !state.jobs.is_empty() && ui.button("Cancel current renders").clicked() {
                self.send(Command::CancelJobs);
            }
        });
        ui.add_space(22.0);
        ui.separator();
        ui.add_space(12.0);
        if state.jobs.is_empty() {
            ui.label("Your current renders will appear here.");
        }
        for job in &state.jobs {
            ui.horizontal(|ui| {
                ui.label(RichText::new(short_id(&job.id)).monospace());
                ui.label(&job.status);
            });
            ui.add(
                egui::ProgressBar::new(job.progress as f32 / 100.0)
                    .text(format!("{}%", job.progress))
                    .fill(BLUE),
            );
            ui.add_space(10.0);
        }
    }

    fn settings_page(&mut self, ui: &mut egui::Ui, state: &AppState) {
        heading(
            ui,
            "SETTINGS.",
            " ",
        );
        ui.label(RichText::new("RENDER CAPACITY").color(BLUE).small());
        ui.horizontal(|ui| {
            ui.label("Maximum simultaneous renders");
            ui.add(
                egui::DragValue::new(&mut self.settings.capacity)
                    .range(1..=u16::MAX)
                    .speed(1),
            );
        });
        ui.label("Default: 10");
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label("Frame renderer");
            ui.selectable_value(&mut self.settings.render_backend, RenderBackend::Gpu, "GPU");
            ui.selectable_value(&mut self.settings.render_backend, RenderBackend::Cpu, "CPU");
        });
        ui.label("Default: GPU, Save to apply");
        ui.add_space(20.0);
        ui.label(RichText::new("DESKTOP").color(BLUE).small());
        ui.checkbox(
            &mut self.settings.autostart,
            "Start Ferris on sign in",
        );
        ui.add_enabled_ui(self.tray.is_some(), |ui| {
            ui.checkbox(
                &mut self.settings.minimize_to_tray,
                "Keep running in the system tray when the window closes",
            );
            ui.checkbox(
                &mut self.settings.start_minimized,
                "Start with the window hidden",
            );
        });
        if self.tray.is_none() {
            ui.label("A system tray is unavailable in this desktop session. Ferris will keep its window accessible.");
        }
        ui.add_space(20.0);
        ui.label(RichText::new("PRIVACY").color(BLUE).small());
        ui.checkbox(
            &mut self.settings.share_machine_info,
            "Also share my PC username and hostname with Ferris",
        );
        ui.add_space(20.0);
        ui.label(RichText::new("FERRIS SERVER").color(BLUE).small());
        ui.add_enabled(
            state.jobs.is_empty() && state.connection != ConnectionState::Connected,
            egui::TextEdit::singleline(&mut self.settings.server_url).desired_width(f32::INFINITY),
        );
        ui.label("The renderer and update signing keys are pinned at build time. Changing this URL does not change the servers those keys trust.");
        ui.add_space(18.0);
        if ui.add(primary("Save settings")).clicked() {
            let validation = ferris_client::api::Api::new(&self.settings.server_url).map(|_| ());
            match validation.and_then(|_| platform::set_autostart(self.settings.autostart)) {
                Ok(()) => {
                    self.send(Command::SaveSettings(self.settings.clone()));
                    self.message = "Saving settings…".into();
                }
                Err(error) => self.message = error.to_string(),
            }
        }
        ui.add_space(24.0);
        ui.label(RichText::new("RENDER TOOLS & UPDATES").color(BLUE).small());
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !state.runtime.installing,
                    egui::Button::new("Install / update render tools"),
                )
                .clicked()
            {
                self.send(Command::InstallRuntime);
            }
            if ui.button("Check client updates").clicked() {
                self.send(Command::CheckUpdates);
            }
        });
        if let Some(path) = &state.runtime.ffmpeg_path {
            ui.label(format!("FFmpeg: {}", path.display()));
            if ui
                .button("Add this FFmpeg directory to my user PATH")
                .clicked()
            {
                self.message = match path
                    .parent()
                    .map(|parent| platform::add_user_path(parent.to_owned()))
                {
                    Some(Ok(())) => {
                        "User PATH updated. New terminals will see it after signing out and in."
                            .into()
                    }
                    Some(Err(error)) => error.to_string(),
                    None => "The FFmpeg directory is unavailable.".into(),
                };
            }
        }
        if let Some(version) = &state.update_available {
            ui.label(format!("Client update available: {version}"));
            if ui.button("Download verified installer").clicked() {
                self.send(Command::DownloadClientUpdate);
            }
        }
        if let Some(path) = &state.update_path {
            ui.label(format!("Downloaded: {}", path.display()));
            if ui.button("Open downloaded installer").clicked() {
                self.message = open::that(path)
                    .map(|_| {
                        "Installer opened. Close Ferris before completing the installation.".into()
                    })
                    .unwrap_or_else(|error| error.to_string());
            }
        }
        ui.add_space(18.0);
        if state.profile.is_some() && ui.button("Sign out and revoke this node").clicked() {
            self.send(Command::SignOut);
            self.consent = false;
        }
    }

    fn activity_page(&mut self, ui: &mut egui::Ui, state: &AppState) {
        heading(
            ui,
            "History",
            "Recent results, transfers and errors from this computer.",
        );
        ui.heading("Render history");
        if state.history.is_empty() {
            ui.label("No completed renders yet.");
        }
        for entry in state.history.iter().take(30) {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(short_id(&entry.id)).monospace().color(BLUE));
                ui.label(&entry.result);
                ui.small(&entry.finished_at);
            });
        }
        ui.add_space(20.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.heading("Logs");
            ui.add(
                egui::TextEdit::singleline(&mut self.log_filter)
                    .hint_text("Filter logs")
                    .desired_width(220.0),
            );
        });
        let filter = self.log_filter.to_lowercase();
        for entry in state
            .logs
            .iter()
            .rev()
            .filter(|entry| {
                filter.is_empty()
                    || entry.message.to_lowercase().contains(&filter)
                    || entry.level.to_lowercase().contains(&filter)
            })
            .take(150)
        {
            ui.horizontal_wrapped(|ui| {
                ui.small(&entry.timestamp);
                ui.label(RichText::new(&entry.level).color(
                    if entry.level.eq_ignore_ascii_case("error") {
                        PINK
                    } else {
                        BLUE
                    },
                ));
                ui.label(&entry.message);
            });
            ui.add_space(5.0);
        }
    }

    fn about_page(&mut self, ui: &mut egui::Ui) {
        heading(
            ui,
            "Abouyt",
            "Ferris Client / Windows and Linux",
        );
        ui.label("This is a virus");
        ui.add_space(16.0);
        ui.label("nah jk");
        ui.add_space(16.0);
        ui.label("Release downloads and scene leases are checked with a pinned signing key and SHA-256. The worker receives prepared drawing commands and media. Replay processing stays on the Ferris server; your sign-in credential stays in the client.");
        ui.add_space(22.0);
        if ui.button("Open Ferris").clicked() {
            self.open("https://ferrisfun.win");
        }
        ui.label(
            RichText::new(format!(
                "Version {CLIENT_VERSION} / {}",
                ferris_client::model::platform_name()
            ))
            .color(BLUE),
        );
    }

    fn open(&mut self, url: &str) {
        if let Err(error) = open::that(url) {
            self.message = format!("Could not open your browser: {error}");
        }
    }
}

impl eframe::App for FerrisApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        let state = self
            .controller
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some(auth) = &state.device_auth {
            if self.opened_auth_code.as_deref() != Some(&auth.user_code) {
                self.opened_auth_code = Some(auth.user_code.clone());
                self.open(&auth.verification_uri);
            }
        } else {
            self.opened_auth_code = None;
        }
        if !self.settings_loaded {
            self.settings = state.settings.clone();
            self.settings_loaded = true;
        }
        if let Some(action) = self.tray.as_ref().and_then(SystemTray::poll) {
            match action {
                TrayAction::Show => {
                    context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    context.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayAction::Drain => self.send(Command::Drain),
                TrayAction::Quit => {
                    self.exit_confirm = true;
                    context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                }
            }
        }
        if context.input(|input| input.viewport().close_requested()) && !self.exit_requested {
            if state.settings.minimize_to_tray && self.tray.is_some() {
                context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                context.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            } else if !state.jobs.is_empty() {
                context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.exit_confirm = true;
            } else {
                self.send(Command::Drain);
            }
        }
        self.sidebar(context, &state);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BACKGROUND).inner_margin(30.0))
            .show(context, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| match self.page {
                    Page::Node => self.node_page(ui, &state),
                    Page::Settings => self.settings_page(ui, &state),
                    Page::Activity => self.activity_page(ui, &state),
                    Page::About => self.about_page(ui),
                });
                if !self.message.is_empty() {
                    ui.add_space(12.0);
                    ui.label(RichText::new(&self.message).color(PINK));
                }
                if self.page != Page::Node {
                    ui.add_space(8.0);
                    ui.label(&state.message);
                }
            });
        if self.exit_confirm {
            egui::Window::new("Close Ferris?").collapsible(false).resizable(false).show(context, |ui| {
                ui.label("Current renders can finish in the background, or you can cancel them and quit.");
                ui.horizontal(|ui| {
                    if ui.button("Keep running").clicked() { self.exit_confirm = false; }
                    if ui.button("Finish and pause").clicked() { self.send(Command::Drain); self.exit_confirm = false; }
                    if ui.button("Cancel renders and quit").clicked() { self.send(Command::CancelJobs); self.send(Command::Drain); self.exit_requested = true; self.exit_confirm = false; }
                });
            });
        }
        if self.exit_requested && state.jobs.is_empty() {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        context.request_repaint_after(Duration::from_millis(250));
    }
}

fn configure_theme(context: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(WHITE);
    visuals.panel_fill = BACKGROUND;
    visuals.window_fill = BACKGROUND;
    visuals.extreme_bg_color = BACKGROUND;
    visuals.faint_bg_color = BACKGROUND;
    visuals.selection.bg_fill = PINK.gamma_multiply(0.22);
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, PINK);
    visuals.hyperlink_color = BLUE;
    visuals.widgets.noninteractive.bg_fill = BACKGROUND;
    visuals.widgets.noninteractive.bg_stroke =
        egui::Stroke::new(1.0_f32, WHITE.gamma_multiply(0.14));
    visuals.widgets.inactive.bg_fill = BACKGROUND;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, WHITE.gamma_multiply(0.25));
    visuals.widgets.hovered.bg_fill = BLUE.gamma_multiply(0.15);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, BLUE);
    visuals.widgets.active.bg_fill = PINK.gamma_multiply(0.2);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, PINK);
    context.set_visuals(visuals);
    context.style_mut(|style| {
        style.spacing.item_spacing = Vec2::new(12.0, 10.0);
        style.spacing.button_padding = Vec2::new(14.0, 10.0);
    });
}

fn heading(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(title).size(29.0).strong());
    ui.label(subtitle);
    ui.add_space(28.0);
}

fn primary(label: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).strong().color(BACKGROUND))
        .fill(PINK)
        .min_size(Vec2::new(170.0, 42.0))
}
fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}
fn metric(ui: &mut egui::Ui, label: &str, value: &str, color: Color32) {
    ui.allocate_ui(Vec2::new(180.0, 85.0), |ui| {
        ui.label(RichText::new(label).size(10.0).color(BLUE));
        ui.label(RichText::new(value).size(25.0).strong().color(color));
    });
}
fn connection_label(connection: &ConnectionState) -> &'static str {
    match connection {
        ConnectionState::Offline => "Offline",
        ConnectionState::SigningIn => "Signing in",
        ConnectionState::Ready => "Ready",
        ConnectionState::Connected => "Contributing",
        ConnectionState::Draining => "Finishing",
        ConnectionState::Error => "Needs attention",
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1080.0, 760.0])
            .with_min_inner_size([860.0, 620.0])
            .with_icon(egui::IconData {
                rgba: platform::icon_pixels(64),
                width: 64,
                height: 64,
            }),
        ..Default::default()
    };
    eframe::run_native(
        "Ferris Client",
        options,
        Box::new(|context| Ok(Box::new(FerrisApp::new(context)))),
    )
}
