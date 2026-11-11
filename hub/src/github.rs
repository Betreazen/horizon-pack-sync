//! Reads builds from the releases of the private pack repository.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::header::{ACCEPT, AUTHORIZATION, LOCATION, USER_AGENT};
use reqwest::{Client, StatusCode, redirect};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::source::{PackSource, SignedManifest, SourceError};

const API_BASE: &str = "https://api.github.com";
const CLIENT_NAME: &str = "pack-sync-hub";
const JSON_MEDIA: &str = "application/vnd.github+json";
const BINARY_MEDIA: &str = "application/octet-stream";
const TAG_PREFIX: &str = "pack-";
const MANIFEST_ASSET: &str = "manifest.signed.json";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// How long "the newest build" may be stale; every server check would otherwise reach GitHub.
const LATEST_TTL: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct ReleaseJson {
    tag_name: String,
    assets: Vec<AssetJson>,
}

#[derive(Deserialize)]
struct AssetJson {
    id: u64,
    name: String,
}

#[derive(Clone)]
struct Build {
    signed: SignedManifest,
    assets: BTreeMap<String, u64>,
}

#[derive(Default)]
struct Cache {
    /// A published build never changes, so it stays cached.
    builds: BTreeMap<i64, Build>,
    latest: Option<(Instant, Option<i64>)>,
}

pub struct GitHubSource {
    client: Client,
    api_base: String,
    repo: String,
    token: String,
    cache: Mutex<Cache>,
}

impl GitHubSource {
    pub fn new(repo: String, token: String) -> Result<Self, reqwest::Error> {
        Self::with_base(API_BASE.to_owned(), repo, token)
    }

    fn with_base(api_base: String, repo: String, token: String) -> Result<Self, reqwest::Error> {
        let client = Client::builder()
            // Redirects carry the file's temporary link, which is the thing we want to read.
            .redirect(redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()?;
        Ok(Self {
            client,
            api_base,
            repo,
            token,
            cache: Mutex::default(),
        })
    }

    fn get(&self, path: &str, media: &str) -> reqwest::RequestBuilder {
        self.client
            .get(format!("{}/repos/{}/{path}", self.api_base, self.repo))
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, CLIENT_NAME)
            .header(ACCEPT, media)
    }

    async fn release_json(&self, path: &str) -> Result<Option<ReleaseJson>, SourceError> {
        let response = self
            .get(&format!("releases/{path}"), JSON_MEDIA)
            .send()
            .await?;
        match response.status() {
            StatusCode::NOT_FOUND => Ok(None),
            StatusCode::OK => Ok(Some(response.json().await?)),
            other => Err(SourceError::Status(other)),
        }
    }

    async fn asset_location(&self, id: u64) -> Result<String, SourceError> {
        let response = self
            .get(&format!("releases/assets/{id}"), BINARY_MEDIA)
            .send()
            .await?;
        if !response.status().is_redirection() {
            return Err(SourceError::Status(response.status()));
        }
        response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .ok_or(SourceError::NoLink)
    }

    async fn build_from(&self, release: ReleaseJson) -> Result<Build, SourceError> {
        let assets: BTreeMap<String, u64> = release
            .assets
            .into_iter()
            .map(|asset| (asset.name, asset.id))
            .collect();
        let id = *assets
            .get(MANIFEST_ASSET)
            .ok_or_else(|| SourceError::NoManifest(release.tag_name.clone()))?;
        let link = self.asset_location(id).await?;
        let response = self
            .client
            .get(link)
            .header(USER_AGENT, CLIENT_NAME)
            .send()
            .await?;
        if response.status() != StatusCode::OK {
            return Err(SourceError::Status(response.status()));
        }
        Ok(Build {
            signed: response.json().await?,
            assets,
        })
    }

    async fn cached_build(&self, sequence: i64) -> Result<Option<Build>, SourceError> {
        if let Some(build) = self.cache.lock().await.builds.get(&sequence) {
            return Ok(Some(build.clone()));
        }
        let Some(release) = self
            .release_json(&format!("tags/{TAG_PREFIX}{sequence}"))
            .await?
        else {
            return Ok(None);
        };
        let build = self.build_from(release).await?;
        self.cache
            .lock()
            .await
            .builds
            .insert(sequence, build.clone());
        Ok(Some(build))
    }

    async fn latest_sequence(&self) -> Result<Option<i64>, SourceError> {
        if let Some((fetched, sequence)) = self.cache.lock().await.latest
            && fetched.elapsed() < LATEST_TTL
        {
            return Ok(sequence);
        }
        let release = self.release_json("latest").await?;
        let sequence = release.and_then(|release| {
            release
                .tag_name
                .strip_prefix(TAG_PREFIX)?
                .parse::<i64>()
                .ok()
        });
        self.cache.lock().await.latest = Some((Instant::now(), sequence));
        Ok(sequence)
    }
}

#[async_trait]
impl PackSource for GitHubSource {
    async fn latest(&self) -> Result<Option<SignedManifest>, SourceError> {
        match self.latest_sequence().await? {
            Some(sequence) => self.release(sequence).await,
            None => Ok(None),
        }
    }

    async fn release(&self, sequence: i64) -> Result<Option<SignedManifest>, SourceError> {
        Ok(self.cached_build(sequence).await?.map(|build| build.signed))
    }

    async fn asset_link(&self, sequence: i64, asset: &str) -> Result<Option<String>, SourceError> {
        let Some(build) = self.cached_build(sequence).await? else {
            return Ok(None);
        };
        // Only names GitHub listed for this build get as far as a request.
        match build.assets.get(asset) {
            Some(id) => self.asset_location(*id).await.map(Some),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use axum::Router;
    use axum::extract::{Request, State};
    use axum::http::{HeaderValue, StatusCode as AxumStatus};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;

    use super::*;

    const TOKEN: &str = "secret-token";

    #[derive(Clone)]
    struct Fake {
        base: String,
        release_calls: Arc<AtomicU32>,
        file_auth: Arc<std::sync::Mutex<Option<String>>>,
    }

    fn release_body(tag: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","assets":[
                {{"id":1,"name":"manifest.signed.json"}},{{"id":2,"name":"client.exe.gz"}}]}}"#
        )
    }

    fn json(body: String) -> Response {
        ([("content-type", "application/json")], body).into_response()
    }

    fn authorized(request: &Request) -> bool {
        request.headers().get("authorization")
            == Some(&HeaderValue::from_str(&format!("Bearer {TOKEN}")).expect("value"))
    }

    fn redirect_to(location: String) -> Response {
        (AxumStatus::FOUND, [("location", location)]).into_response()
    }

    async fn latest(State(fake): State<Fake>, request: Request) -> Response {
        fake.release_calls.fetch_add(1, Ordering::SeqCst);
        if !authorized(&request) {
            return AxumStatus::UNAUTHORIZED.into_response();
        }
        json(release_body("pack-4"))
    }

    async fn tagged(request: Request) -> Response {
        if request.uri().path().ends_with("pack-4") {
            json(release_body("pack-4"))
        } else {
            AxumStatus::NOT_FOUND.into_response()
        }
    }

    async fn asset(State(fake): State<Fake>, request: Request) -> Response {
        let id = request
            .uri()
            .path()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        redirect_to(format!("{}/signed-file/{id}", fake.base))
    }

    async fn signed_file(State(fake): State<Fake>, request: Request) -> Response {
        let auth = request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        *fake.file_auth.lock().expect("lock") = auth;
        json(r#"{"manifest":"m","signature":"s"}"#.to_owned())
    }

    async fn start() -> (GitHubSource, Fake) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let fake = Fake {
            base: base.clone(),
            release_calls: Arc::default(),
            file_auth: Arc::default(),
        };
        let app = Router::new()
            .route("/repos/o/r/releases/latest", get(latest))
            .route("/repos/o/r/releases/tags/{tag}", get(tagged))
            .route("/repos/o/r/releases/assets/{id}", get(asset))
            .route("/signed-file/{id}", get(signed_file))
            .with_state(fake.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
        let source = GitHubSource::with_base(base, "o/r".into(), TOKEN.into()).expect("client");
        (source, fake)
    }

    #[tokio::test]
    async fn the_newest_release_is_read_through_its_manifest_asset() {
        let (source, _) = start().await;
        let signed = source.latest().await.expect("ok").expect("found");
        assert_eq!(signed.manifest, "m");
    }

    #[tokio::test]
    async fn the_token_is_not_sent_to_the_file_host() {
        let (source, fake) = start().await;
        source.release(4).await.expect("ok").expect("found");
        assert_eq!(*fake.file_auth.lock().expect("lock"), None);
    }

    #[tokio::test]
    async fn repeated_checks_reach_github_only_once() {
        let (source, fake) = start().await;
        source.latest().await.expect("ok");
        source.latest().await.expect("ok");
        assert_eq!(fake.release_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_missing_release_is_none() {
        let (source, _) = start().await;
        assert_eq!(source.release(9).await.expect("ok"), None);
    }

    #[tokio::test]
    async fn a_link_is_given_only_for_a_file_the_build_lists() {
        let (source, fake) = start().await;
        let link = source
            .asset_link(4, "client.exe.gz")
            .await
            .expect("ok")
            .expect("found");
        assert_eq!(link, format!("{}/signed-file/2", fake.base));
        assert_eq!(source.asset_link(4, "../../evil").await.expect("ok"), None);
    }
}
