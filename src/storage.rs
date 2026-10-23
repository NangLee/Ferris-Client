use crate::model::{HistoryEntry, Settings};
use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub struct Storage {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
}

impl Storage {
    pub fn open() -> Result<Self> {
        let dirs = ProjectDirs::from("win", "Ferris", "Ferris Client")
            .context("Could not locate your application data directory")?;
        let storage = Self {
            config: dirs.config_dir().to_owned(),
            data: dirs.data_local_dir().to_owned(),
            cache: dirs.cache_dir().to_owned(),
        };
        for directory in [&storage.config, &storage.data, &storage.cache] {
            fs::create_dir_all(directory)?;
            private_directory(directory)?;
        }
        Ok(storage)
    }

    pub fn settings(&self) -> Settings {
        read_json(&self.config.join("settings.json")).unwrap_or_default()
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        write_json(&self.config.join("settings.json"), settings)
    }

    pub fn history(&self) -> Vec<HistoryEntry> {
        read_json(&self.data.join("history.json")).unwrap_or_default()
    }

    pub fn save_history(&self, history: &[HistoryEntry]) -> Result<()> {
        write_json(&self.data.join("history.json"), history)
    }

    pub fn append_log(&self, line: &str) -> Result<()> {
        let path = self.data.join("client.log");
        if fs::metadata(&path).is_ok_and(|m| m.len() > 2 * 1024 * 1024) {
            let previous = self.data.join("client.previous.log");
            if previous.exists() {
                fs::remove_file(&previous)?;
            }
            fs::rename(&path, previous)?;
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        private_file(&path)?;
        writeln!(file, "{}", redact(line))?;
        Ok(())
    }

    pub fn keyring_entry(&self, server_url: &str) -> Result<keyring::Entry> {
        let account = format!("node-{:x}", Sha256::digest(server_url.as_bytes()));
        keyring::Entry::new("Ferris Client", &account)
            .context("The operating system credential vault is unavailable")
    }

    pub fn load_token(&self, server_url: &str) -> Result<Option<String>> {
        match self.keyring_entry(server_url)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => {
                Err(error).context("Unlock your operating system credential vault to sign in")
            }
        }
    }

    pub fn save_token(&self, server_url: &str, token: &str) -> Result<()> {
        self.keyring_entry(server_url)?
            .set_password(token)
            .context("Could not save your sign-in in the operating system credential vault")
    }

    pub fn delete_token(&self, server_url: &str) -> Result<()> {
        match self.keyring_entry(server_url)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error)
                .context("Could not remove the sign-in from the operating system credential vault"),
        }
    }
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path)?;
    if bytes.len() > 2 * 1024 * 1024 {
        bail!("Stored configuration is too large");
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn write_json<T: serde::Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(value)?;
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        private_file(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        // Rust uses an atomic replacement rename on Windows and Unix. Keep the
        // old configuration intact until the entire replacement is durable.
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if temporary.exists() {
        let _ = fs::remove_file(temporary);
    }
    result
}

pub fn private_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let _ = path;
    Ok(())
}

pub fn private_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    let _ = path;
    Ok(())
}

pub fn redact(line: &str) -> String {
    line.split_whitespace()
        .map(|word| {
            if word.contains("http://") || word.contains("https://") {
                "[url]".to_owned()
            } else if word.len() >= 40 || word.to_ascii_lowercase().starts_with("bearer") {
                "[redacted]".to_owned()
            } else {
                word.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
