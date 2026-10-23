use anyhow::{Context, Result, bail};
#[cfg(target_os = "linux")]
use std::fs;
use std::{path::PathBuf, process::Command};
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem},
};

pub enum TrayAction {
    Show,
    Drain,
    Quit,
}

pub struct SystemTray {
    _icon: TrayIcon,
    show: MenuItem,
    drain: MenuItem,
    quit: MenuItem,
}

impl SystemTray {
    pub fn new() -> Result<Self> {
        let menu = Menu::new();
        let show = MenuItem::new("Open Ferris", true, None);
        let drain = MenuItem::new("Finish current renders and pause", true, None);
        let quit = MenuItem::new("Quit Ferris", true, None);
        menu.append_items(&[&show, &drain, &quit])?;
        let icon = TrayIconBuilder::new()
            .with_tooltip("Ferris Client")
            .with_icon(Icon::from_rgba(icon_pixels(32), 32, 32)?)
            .with_menu(Box::new(menu))
            .build()?;
        Ok(Self {
            _icon: icon,
            show,
            drain,
            quit,
        })
    }

    pub fn poll(&self) -> Option<TrayAction> {
        if let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == *self.show.id() {
                return Some(TrayAction::Show);
            }
            if event.id == *self.drain.id() {
                return Some(TrayAction::Drain);
            }
            if event.id == *self.quit.id() {
                return Some(TrayAction::Quit);
            }
        }
        if let Ok(TrayIconEvent::Click { .. } | TrayIconEvent::DoubleClick { .. }) =
            TrayIconEvent::receiver().try_recv()
        {
            return Some(TrayAction::Show);
        }
        None
    }
}

pub fn icon_pixels(size: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let px = x as f32 / size as f32;
            let py = y as f32 / size as f32;
            let left = (px - 0.34).powi(2) + (py - 0.5).powi(2) < 0.27_f32.powi(2);
            let right = (px - 0.67).powi(2) + (py - 0.5).powi(2) < 0.24_f32.powi(2);
            pixels.extend_from_slice(if left {
                &[255, 107, 139, 255]
            } else if right {
                &[110, 208, 255, 255]
            } else {
                &[32, 32, 38, 255]
            });
        }
    }
    pixels
}

pub fn set_autostart(enabled: bool) -> Result<()> {
    let executable = std::env::current_exe()?;
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("reg.exe");
        if enabled {
            command
                .args([
                    "add",
                    r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                    "/v",
                    "FerrisClient",
                    "/t",
                    "REG_SZ",
                    "/d",
                ])
                .arg(format!("\"{}\" --minimized", executable.display()))
                .arg("/f");
        } else {
            command.args([
                "delete",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "FerrisClient",
                "/f",
            ]);
        }
        hide_console(&mut command);
        let result = command
            .output()
            .context("Could not update Windows startup settings")?;
        if enabled && !result.status.success() {
            bail!("Windows could not save the startup entry");
        }
    }
    #[cfg(target_os = "linux")]
    {
        let paths = directories::BaseDirs::new().context("No user configuration directory")?;
        let directory = paths.config_dir().join("autostart");
        let destination = directory.join("ferris-client.desktop");
        if enabled {
            fs::create_dir_all(&directory)?;
            let quoted = executable
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('`', "\\`")
                .replace('$', "\\$")
                .replace('%', "%%");
            fs::write(
                destination,
                format!(
                    "[Desktop Entry]\nType=Application\nName=Ferris Client\nExec=\"{quoted}\" --minimized\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
                ),
            )?;
        } else if destination.exists() {
            fs::remove_file(destination)?;
        }
    }
    Ok(())
}

pub fn add_user_path(directory: PathBuf) -> Result<()> {
    let directory = directory
        .canonicalize()
        .context("The runtime directory does not exist")?;
    #[cfg(target_os = "windows")]
    {
        // Encoded arguments avoid interpolating file paths into PowerShell source.
        let path_json = serde_json::to_string(&directory.to_string_lossy())?;
        let path_b64 =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, path_json);
        let script = format!(
            "$p=ConvertFrom-Json ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{path_b64}'))); $current=[Environment]::GetEnvironmentVariable('Path','User'); $entries=@($current -split ';' | Where-Object {{ $_ }}); if ($entries -notcontains $p) {{ [Environment]::SetEnvironmentVariable('Path',(($entries + $p) -join ';'),'User') }}"
        );
        let mut command = Command::new("powershell.exe");
        command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
        hide_console(&mut command);
        if !command.status()?.success() {
            bail!("Could not update your user PATH");
        }
    }
    #[cfg(target_os = "linux")]
    {
        let paths = directories::BaseDirs::new().context("No user configuration directory")?;
        let shell_path = directory
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('`', "\\`")
            .replace('$', "\\$");
        let config = paths.config_dir().join("environment.d");
        fs::create_dir_all(&config)?;
        fs::write(
            config.join("90-ferris-ffmpeg.conf"),
            format!("PATH=\"{shell_path}:${{PATH}}\"\n"),
        )?;
    }
    Ok(())
}

pub fn hide_console(command: &mut Command) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    #[cfg(not(target_os = "windows"))]
    let _ = command;
}
