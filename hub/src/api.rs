//! The three routes pack-sync talks to.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;

use crate::client_ip;
use crate::codes::hash_code;
use crate::config::PackConfig;
use crate::github::GitHubSource;
use crate::limits::RequestLimiter;
use crate::source::{PackSource, SignedManifest, SourceError};
use crate::store::{AccessStore, StoreError};

const BEARER_PREFIX: &str = "Bearer ";
/// Players get only these files: the server build carries resources made from the game's files.
const PLAYER_ASSET_PREFIX: &str = "client-";

struct Inner {
    source: Arc<dyn PackSource>,
    store: Arc<dyn AccessStore>,
    server_key_hash: String,
    limiter: RequestLimiter,
}

/// Everything the routes share.
#[derive(Clone)]
pub struct PackApi {
    inner: Arc<Inner>,
}

impl PackApi {
    pub fn new(source: Arc<dyn PackSource>, store: Arc<dyn AccessStore>, server_key: &str) -> Self {
        Self {
            inner: Arc::new(Inner {
                source,
                store,
                server_key_hash: hash_code(server_key),
                limiter: RequestLimiter::default(),
            }),
        }
    }

    /// Reads builds from the GitHub repository the configuration names.
    pub fn from_config(
        config: &PackConfig,
        store: Arc<dyn AccessStore>,
    ) -> Result<Self, reqwest::Error> {
        let source = GitHubSource::new(config.github_repo.clone(), config.github_token.clone())?;
        Ok(Self::new(Arc::new(source), store, &config.server_key))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Caller {
    GameServer,
    Player,
}

#[derive(Debug)]
enum PackError {
    NotFound,
    Forbidden,
    Source(SourceError),
    Store(StoreError),
}

impl From<SourceError> for PackError {
    fn from(error: SourceError) -> Self {
        Self::Source(error)
    }
}

impl From<StoreError> for PackError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl IntoResponse for PackError {
    fn into_response(self) -> Response {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND.into_response(),
            Self::Forbidden => StatusCode::FORBIDDEN.into_response(),
            Self::Source(error) => {
                tracing::error!(error = %error, "cannot read the pack repository");
                StatusCode::BAD_GATEWAY.into_response()
            }
            Self::Store(error) => {
                tracing::error!(error = %error, "pack store error");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix(BEARER_PREFIX)
}

async fn authorize(State(api): State<PackApi>, mut request: Request, next: Next) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(address)| address.ip());
    let ip = client_ip::resolve(request.headers(), peer);
    if !api.inner.limiter.allow(ip) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let Some(hash) = bearer(request.headers()).map(hash_code) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let caller = if hash == api.inner.server_key_hash {
        Caller::GameServer
    } else {
        match api.inner.store.is_player(&hash).await {
            Ok(true) => Caller::Player,
            Ok(false) => return StatusCode::UNAUTHORIZED.into_response(),
            Err(error) => return PackError::Store(error).into_response(),
        }
    };
    request.extensions_mut().insert(caller);
    next.run(request).await
}

/// The game server sees the newest build; players see only what the server already runs.
async fn manifest(
    State(api): State<PackApi>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<SignedManifest>, PackError> {
    let source = &api.inner.source;
    let found = match caller {
        Caller::GameServer => source.latest().await?,
        Caller::Player => match api.inner.store.confirmed().await? {
            Some(sequence) => source.release(sequence).await?,
            None => None,
        },
    };
    found.map(Json).ok_or(PackError::NotFound)
}

async fn file(
    State(api): State<PackApi>,
    Extension(caller): Extension<Caller>,
    Path((sequence, asset)): Path<(i64, String)>,
) -> Result<Redirect, PackError> {
    if caller == Caller::Player {
        let is_allowed = asset.starts_with(PLAYER_ASSET_PREFIX)
            && api.inner.store.confirmed().await? == Some(sequence);
        if !is_allowed {
            return Err(PackError::NotFound);
        }
    }
    let link = api.inner.source.asset_link(sequence, &asset).await?;
    link.map(|link| Redirect::temporary(&link))
        .ok_or(PackError::NotFound)
}

#[derive(Deserialize)]
struct Confirmation {
    sequence: i64,
}

async fn confirm(
    State(api): State<PackApi>,
    Extension(caller): Extension<Caller>,
    Json(body): Json<Confirmation>,
) -> Result<StatusCode, PackError> {
    if caller != Caller::GameServer {
        return Err(PackError::Forbidden);
    }
    if api.inner.source.release(body.sequence).await?.is_none() {
        return Err(PackError::NotFound);
    }
    api.inner.store.set_confirmed(body.sequence).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The routes, ready to merge into any axum router. Serve it with
/// `into_make_service_with_connect_info::<SocketAddr>()` so the limit sees real addresses.
pub fn router<S: Clone + Send + Sync + 'static>(api: PackApi) -> Router<S> {
    Router::new()
        .route("/api/pack/manifest", get(manifest))
        .route("/api/pack/file/{sequence}/{asset}", get(file))
        .route("/api/pack/confirm", post(confirm))
        .route_layer(middleware::from_fn_with_state(api.clone(), authorize))
        .with_state(api)
}
