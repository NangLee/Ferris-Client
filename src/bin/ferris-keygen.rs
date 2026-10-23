//! Operator utility. It prints paths and the public key, never the private seed.
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::SigningKey;
use std::{fs, io::Write, path::PathBuf};

fn main() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let private = root.join(".private");
    fs::create_dir_all(&private)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700))?;
    }
    let secret_path = private.join("node-signing-secret.env");
    let seed: [u8; 32] = if secret_path.exists() {
        let existing = fs::read_to_string(&secret_path)?;
        let encoded = existing
            .trim()
            .strip_prefix("NODE_SIGNING_SECRET=")
            .context("Unexpected existing key format; it was not changed")?;
        let bytes = STANDARD.decode(encoded)?;
        if bytes.len() != 32 {
            bail!("The existing key is not a 32-byte seed; it was not changed");
        }
        bytes.try_into().unwrap()
    } else {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|error| {
            anyhow::anyhow!("The operating system could not generate a key: {error}")
        })?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&secret_path)?;
        writeln!(file, "NODE_SIGNING_SECRET={}", STANDARD.encode(seed))?;
        file.sync_all()?;
        seed
    };
    let public = SigningKey::from_bytes(&seed).verifying_key();
    let public_path = root.join("public-signing-key.txt");
    fs::write(
        &public_path,
        format!("{}\n", STANDARD.encode(public.as_bytes())),
    )?;
    println!(
        "Private key: {} (ignored by Git; keep private)",
        secret_path.display()
    );
    println!("Public key: {}", public_path.display());
    Ok(())
}
