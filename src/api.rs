use crate::model::{DeviceAuth, Profile, SlotsRequest, SlotsResponse};
use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};
use tokio::io::AsyncWriteExt;
use url::Url;

const JSON_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Clone)]
pub struct Api {
    pub base: Url,
    client: Client,
}

#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub node_id: serde_json::Value,
    pub user: Profile,
}

impl Api {
    pub fn new(server: &str) -> Result<Self> {
        let mut base = Url::parse(server.trim()).context("Enter a complete Ferris server URL")?;
        validate_transport(&base)?;
        if base.username() != ""
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            bail!("The server address must not include credentials, a query, or a fragment");
        }
        if base.path() != "/" && base.path() != "" {
            bail!("Use the Ferris site's root address");
        }
        base.set_path("/");
        let client = Client::builder()
            .https_only(base.scheme() == "https")
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(format!("Ferris-Client/{}", crate::model::CLIENT_VERSION))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(45))
            .build()?;
        Ok(Self { base, client })
    }

    pub fn same_origin_url(&self, path: &str) -> Result<Url> {
        let url = self.base.join(path).context("Invalid server endpoint")?;
        if url.origin() != self.base.origin()
            || url.username() != ""
            || url.password().is_some()
            || url.fragment().is_some()
        {
            bail!("Ferris supplied an endpoint on a different server");
        }
        Ok(url)
    }

    pub fn endpoint(&self, path: &str) -> Result<Url> {
        self.same_origin_url(&format!("/api/vnone/{}", path.trim_start_matches('/')))
    }

    pub async fn begin_sign_in(&self) -> Result<DeviceAuth> {
        let response = self
            .client
            .post(self.endpoint("auth/device")?)
            .json(&serde_json::json!({"client_name":"Ferris Client"}))
            .send()
            .await
            .context("Could not contact Ferris to start sign-in")?;
        let mut auth: DeviceAuth = json_response(response).await?;
        auth.verification_uri = self.same_origin_url(&auth.verification_uri)?.to_string();
        if auth.device_code.len() < 16 || auth.device_code.len() > 1024 || auth.user_code.len() > 64
        {
            bail!("Invalid sign-in response");
        }
        Ok(auth)
    }

    pub async fn poll_sign_in(&self, device_code: &str) -> Result<Option<TokenResponse>> {
        let response = self
            .client
            .post(self.endpoint("auth/token")?)
            .json(&serde_json::json!({"device_code":device_code}))
            .send()
            .await
            .context("Could not complete Ferris sign-in")?;
        if response.status() == StatusCode::ACCEPTED {
            return Ok(None);
        }
        let response: TokenResponse = json_response(response).await?;
        if response.access_token.len() < 20 || response.access_token.len() > 4096 {
            bail!("Invalid sign-in credential");
        }
        Ok(Some(response))
    }

    pub async fn profile(&self, token: &str) -> Result<Profile> {
        let response = self
            .client
            .get(self.endpoint("auth/me")?)
            .bearer_auth(token)
            .send()
            .await
            .context("Could not refresh your Ferris account")?;
        #[derive(Deserialize)]
        struct UserResponse {
            user: Profile,
        }
        Ok(json_response::<UserResponse>(response).await?.user)
    }

    pub async fn revoke(&self, token: &str) -> Result<()> {
        let response = self
            .client
            .post(self.endpoint("auth/revoke")?)
            .bearer_auth(token)
            .send()
            .await
            .context("Could not revoke the node credential")?;
        check_status(&response)?;
        Ok(())
    }

    pub async fn slots(&self, token: &str, body: &SlotsRequest) -> Result<SlotsResponse> {
        let response = self
            .client
            .post(self.endpoint("servers/slots")?)
            .bearer_auth(token)
            .json(body)
            .send()
            .await
            .context("The Ferris heartbeat could not reach the server")?;
        json_response(response).await
    }

    pub async fn scene_permit(
        &self,
        token: &str,
        id: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let id = uuid::Uuid::parse_str(id).context("Invalid scene job ID")?;
        let response = self
            .client
            .post(self.endpoint(&format!("jobs/{id}/permit"))?)
            .bearer_auth(token)
            .json(body)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .context("Could not renew the signed scene lease")?;
        json_response(response).await
    }

    pub async fn progress(
        &self,
        token: &str,
        id: &str,
        lease: &str,
        progress: u8,
        error: Option<&str>,
    ) -> Result<()> {
        let id = uuid::Uuid::parse_str(id).context("Invalid job ID")?;
        let response = self
            .client
            .post(self.endpoint(&format!("jobs/{id}/progress"))?)
            .bearer_auth(token)
            .json(&serde_json::json!({"lease_token":lease,"progress":progress,"error":error}))
            .send()
            .await
            .context("Could not send render progress")?;
        check_status(&response)?;
        Ok(())
    }

    pub async fn complete(&self, token: &str, url: &str, lease: &str, video: &Path) -> Result<()> {
        let size = tokio::fs::metadata(video).await?.len();
        if size == 0 {
            bail!("The renderer produced an empty video");
        }
        if size > 300 * 1024 * 1024 {
            bail!("result_too_large: rendered video exceeds the 300 MB node upload limit");
        }
        let file = tokio::fs::File::open(video).await?;
        let stream = tokio_util::io::ReaderStream::new(file);
        let part =
            reqwest::multipart::Part::stream_with_length(reqwest::Body::wrap_stream(stream), size)
                .file_name("render.mp4")
                .mime_str("video/mp4")?;
        let form = reqwest::multipart::Form::new()
            .text("lease_token", lease.to_owned())
            .part("video", part);
        let response = self
            .client
            .post(self.same_origin_url(url)?)
            .bearer_auth(token)
            .timeout(Duration::from_secs(1800))
            .multipart(form)
            .send()
            .await
            .context("Could not send the finished render to Ferris")?;
        check_status(&response)?;
        Ok(())
    }

    pub async fn manifest(&self) -> Result<serde_json::Value> {
        let mut url = self.endpoint("client/manifest")?;
        url.query_pairs_mut()
            .append_pair("platform", crate::model::platform_name());
        let response = self
            .client
            .get(url)
            .send()
            .await
            .context("Could not check signed releases")?;
        json_response(response).await
    }

    pub async fn download_asset(
        &self,
        token: &str,
        url: &str,
        target: &Path,
        expected_hash: &str,
        expected_size: u64,
        limit: u64,
    ) -> Result<()> {
        let response = self
            .client
            .get(self.same_origin_url(url)?)
            .bearer_auth(token)
            .timeout(Duration::from_secs(1800))
            .send()
            .await
            .context("Could not download a job asset")?;
        save_verified(response, target, expected_hash, expected_size, limit).await
    }

    pub async fn download_release(
        &self,
        url: &str,
        target: &Path,
        hash: &str,
        size: u64,
    ) -> Result<()> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(1800))
            .build()?;
        let mut url = Url::parse(url).context("Invalid signed release URL")?;
        for _ in 0..6 {
            validate_transport(&url)?;
            if url.scheme() != "https" || url.username() != "" || url.password().is_some() {
                bail!("Release downloads require HTTPS without URL credentials");
            }
            let response = client
                .get(url.clone())
                .send()
                .await
                .context("Could not download the signed release")?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .context("A release redirect has no destination")?
                    .to_str()?;
                url = url.join(location)?;
                continue;
            }
            return save_verified(response, target, hash, size, 2 * 1024 * 1024 * 1024).await;
        }
        bail!("Too many release download redirects")
    }
}

pub fn validate_transport(url: &Url) -> Result<()> {
    if url.scheme() == "https" {
        return Ok(());
    }
    if cfg!(debug_assertions)
        && url.scheme() == "http"
        && matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        )
    {
        return Ok(());
    }
    bail!("Ferris requires HTTPS (debug builds also allow HTTP on localhost)")
}

fn check_status(response: &Response) -> Result<()> {
    let status = response.status();
    if status == StatusCode::UNAUTHORIZED {
        bail!("Sign-in expired (HTTP 401); sign in again");
    }
    if status == StatusCode::FORBIDDEN {
        bail!("Ferris refused this node or job (HTTP 403)");
    }
    if !status.is_success() {
        bail!("Ferris returned HTTP {}", status.as_u16());
    }
    Ok(())
}

async fn json_response<T: DeserializeOwned>(response: Response) -> Result<T> {
    check_status(&response)?;
    if response
        .content_length()
        .is_some_and(|n| n > JSON_LIMIT as u64)
    {
        bail!("The server response is too large");
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("The server response was interrupted")?;
        if bytes.len() + chunk.len() > JSON_LIMIT {
            bail!("The server response is too large");
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).context("Ferris returned an invalid JSON response")
}

async fn save_verified(
    response: Response,
    target: &Path,
    hash: &str,
    size: u64,
    limit: u64,
) -> Result<()> {
    check_status(&response)?;
    if size == 0 || size > limit || hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit())
    {
        bail!("Invalid asset size or SHA-256 checksum");
    }
    if response
        .content_length()
        .is_some_and(|n| n != size || n > limit)
    {
        bail!("The download size does not match its signed metadata");
    }
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)
            .await?;
        crate::storage::private_file(target)?;
        let mut digest = Sha256::new();
        let mut received = 0u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("The asset download was interrupted")?;
            received = received
                .checked_add(chunk.len() as u64)
                .context("The asset is too large")?;
            if received > size || received > limit {
                bail!("The download exceeded its advertised size");
            }
            digest.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.sync_all().await?;
        if received != size || !format!("{:x}", digest.finalize()).eq_ignore_ascii_case(hash) {
            bail!("The download failed SHA-256 verification");
        }
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(target).await;
    }
    result
}
