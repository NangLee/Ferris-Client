//! A scene worker receives media and drawing commands, never Ferris credentials.
//! The client verifies and renews the online lease while supervising its process.
use crate::{api::Api, model::SCENE_FORMAT, runtime};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::Instant;

#[derive(Deserialize)]
struct Permit {
    version: u32,
    audience: String,
    node_id: String,
    job_id: String,
    nonce: String,
    scene_sha256: String,
    scene_format: String,
    issued_at: u64,
    expires_at: u64,
}

pub struct SceneLease {
    api: Api,
    token: String,
    job_id: String,
    lease_token: String,
    scene_sha256: String,
    nonce: String,
    node_id: String,
    deadline: Instant,
    renew_at: Instant,
}

impl SceneLease {
    pub async fn authorize(
        api: &Api,
        token: &str,
        job_id: &str,
        lease_token: &str,
        scene_sha256: &str,
        node_id: &str,
    ) -> Result<Self> {
        uuid::Uuid::parse_str(job_id).context("Invalid scene job ID")?;
        uuid::Uuid::parse_str(node_id).context("Invalid scene node ID")?;
        let mut lease = Self {
            api: api.clone(),
            token: token.into(),
            job_id: job_id.into(),
            lease_token: lease_token.into(),
            scene_sha256: scene_sha256.into(),
            nonce: uuid::Uuid::new_v4().to_string(),
            node_id: node_id.into(),
            deadline: Instant::now(),
            renew_at: Instant::now(),
        };
        lease
            .refresh(false)
            .await
            .context("Ferris did not authorize this scene render")?;
        Ok(lease)
    }

    pub async fn maintain(&mut self) -> Result<()> {
        loop {
            tokio::time::sleep_until(self.renew_at.min(self.deadline)).await;
            if Instant::now() >= self.deadline {
                bail!("The signed scene lease expired; rendering stopped");
            }
            // A failed network request may only consume the time remaining in
            // the currently signed permit. It cannot extend authorization.
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            let result = tokio::time::timeout(remaining, self.refresh(true)).await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) if transient(&error) => {
                    self.renew_at = Instant::now() + Duration::from_secs(5);
                }
                Ok(Err(error)) => {
                    return Err(error).context("The signed scene lease was revoked or invalid");
                }
                Err(_) => bail!("The signed scene lease expired; rendering stopped"),
            }
        }
    }

    pub fn valid(&self) -> bool {
        Instant::now() < self.deadline
    }

    async fn refresh(&mut self, renewal: bool) -> Result<()> {
        let body = serde_json::json!({
            "lease_token":self.lease_token, "nonce":self.nonce,
            "scene_sha256":self.scene_sha256, "scene_format":SCENE_FORMAT, "renewal":renewal,
        });
        let envelope = self
            .api
            .scene_permit(&self.token, &self.job_id, &body)
            .await?;
        let payload = runtime::verify_signed_payload(envelope, 32 * 1024)
            .context("The scene permit signature is invalid")?;
        let permit: Permit =
            serde_json::from_slice(&payload).context("Invalid scene permit payload")?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        if permit.version != 2
            || permit.audience != "ferris-scene"
            || permit.node_id != self.node_id
            || permit.job_id != self.job_id
            || permit.nonce != self.nonce
            || permit.scene_sha256 != self.scene_sha256
            || permit.scene_format != SCENE_FORMAT
            || permit.expires_at <= now
            || permit.expires_at.saturating_sub(now) > 120
            || permit.issued_at > now + 30
            || permit.expires_at <= permit.issued_at
            || permit.expires_at.saturating_sub(permit.issued_at) > 120
        {
            bail!("The signed scene permit does not match this node and job");
        }
        self.deadline = Instant::now() + Duration::from_secs(permit.expires_at - now);
        self.renew_at = Instant::now() + Duration::from_secs(45);
        Ok(())
    }
}

fn transient(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}");
    if text.contains("HTTP 429") {
        return true;
    }
    if text.contains("HTTP ") {
        return text.contains("HTTP 5");
    }
    text.contains("Could not renew the signed scene lease")
}
