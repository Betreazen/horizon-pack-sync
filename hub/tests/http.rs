//! The API against a real router and HTTP client, with GitHub stood in for.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use pack_sync_hub::{
    FileStore, PackApi, PackSource, SignedManifest, SourceError, hash_code, router,
};
use reqwest::StatusCode;
use reqwest::redirect::Policy;
use tempfile::TempDir;

const SERVER_KEY: &str = "game-server-key-0123456789abcdefgh";
const NEWEST: i64 = 5;
const LINK: &str = "https://files.example/temporary";

struct FakeSource;

fn signed(sequence: i64) -> SignedManifest {
    SignedManifest {
        manifest: format!("manifest-{sequence}"),
        signature: "sig".to_owned(),
    }
}

fn exists(sequence: i64) -> bool {
    (1..=NEWEST).contains(&sequence)
}

#[async_trait]
impl PackSource for FakeSource {
    async fn latest(&self) -> Result<Option<SignedManifest>, SourceError> {
        Ok(Some(signed(NEWEST)))
    }

    async fn release(&self, sequence: i64) -> Result<Option<SignedManifest>, SourceError> {
        Ok(exists(sequence).then(|| signed(sequence)))
    }

    async fn asset_link(&self, sequence: i64, asset: &str) -> Result<Option<String>, SourceError> {
        Ok((exists(sequence) && asset.ends_with(".gz")).then(|| LINK.to_owned()))
    }
}

struct Hub {
    base: String,
    http: reqwest::Client,
    store: Arc<FileStore>,
    _dir: TempDir,
}

async fn spawn() -> Hub {
    let dir = TempDir::new().expect("dir");
    let store = Arc::new(FileStore::new(dir.path()));
    let api = PackApi::new(Arc::new(FakeSource), store.clone(), SERVER_KEY);
    let app: Router = router(api);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("serve");
    });
    let http = reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .expect("client");
    Hub {
        base,
        http,
        store,
        _dir: dir,
    }
}

impl Hub {
    async fn player_code(&self) -> String {
        self.store.add_player("player").await.expect("code")
    }

    fn get(&self, path: &str, token: &str) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{}/api/pack/{path}", self.base))
            .bearer_auth(token)
    }

    async fn status(&self, path: &str, token: &str) -> StatusCode {
        self.get(path, token).send().await.expect("send").status()
    }

    async fn confirm(&self, token: &str, sequence: i64) -> StatusCode {
        self.http
            .post(format!("{}/api/pack/confirm", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({ "sequence": sequence }))
            .send()
            .await
            .expect("send")
            .status()
    }

    async fn manifest_of(&self, token: &str) -> SignedManifest {
        let response = self.get("manifest", token).send().await.expect("send");
        assert_eq!(response.status(), StatusCode::OK);
        response.json().await.expect("json")
    }
}

#[tokio::test]
async fn an_unknown_code_gets_nothing() {
    let hub = spawn().await;
    assert_eq!(
        hub.status("manifest", "wrong").await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_request_without_a_code_gets_nothing() {
    let hub = spawn().await;
    let url = format!("{}/api/pack/manifest", hub.base);
    let response = hub.http.get(url).send().await.expect("send");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_game_server_sees_the_newest_build() {
    let hub = spawn().await;
    assert_eq!(hub.manifest_of(SERVER_KEY).await.manifest, "manifest-5");
}

#[tokio::test]
async fn a_player_sees_nothing_until_the_server_confirms_a_build() {
    let hub = spawn().await;
    let code = hub.player_code().await;
    assert_eq!(hub.status("manifest", &code).await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_player_gets_the_confirmed_build_not_the_newest() {
    let hub = spawn().await;
    let code = hub.player_code().await;
    assert_eq!(hub.confirm(SERVER_KEY, 3).await, StatusCode::NO_CONTENT);
    assert_eq!(hub.manifest_of(&code).await.manifest, "manifest-3");
}

#[tokio::test]
async fn a_player_cannot_confirm_a_build() {
    let hub = spawn().await;
    let code = hub.player_code().await;
    assert_eq!(hub.confirm(&code, 3).await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_build_that_does_not_exist_cannot_be_confirmed() {
    let hub = spawn().await;
    assert_eq!(hub.confirm(SERVER_KEY, 99).await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_file_redirects_to_its_temporary_link() {
    let hub = spawn().await;
    let code = hub.player_code().await;
    hub.confirm(SERVER_KEY, 3).await;
    let response = hub
        .get("file/3/client-windows.gz", &code)
        .send()
        .await
        .expect("send");
    assert!(response.status().is_redirection());
    assert_eq!(response.headers()["location"], LINK);
}

#[tokio::test]
async fn a_player_cannot_fetch_a_build_the_server_has_not_confirmed() {
    let hub = spawn().await;
    let code = hub.player_code().await;
    hub.confirm(SERVER_KEY, 3).await;
    let status = hub.status("file/5/client-windows.gz", &code).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_player_cannot_fetch_the_server_build() {
    let hub = spawn().await;
    let code = hub.player_code().await;
    hub.confirm(SERVER_KEY, 3).await;
    let path = "file/3/server-linux.tar.gz";
    assert_eq!(hub.status(path, &code).await, StatusCode::NOT_FOUND);
    assert!(hub.status(path, SERVER_KEY).await.is_redirection());
}

#[tokio::test]
async fn the_game_server_can_fetch_a_build_nobody_confirmed_yet() {
    let hub = spawn().await;
    let status = hub.status("file/5/client-windows.gz", SERVER_KEY).await;
    assert!(status.is_redirection());
}

#[tokio::test]
async fn a_new_code_cancels_the_old_one() {
    let hub = spawn().await;
    let old = hub.player_code().await;
    let new = hub.player_code().await;
    hub.confirm(SERVER_KEY, 3).await;
    assert_eq!(hub.status("manifest", &old).await, StatusCode::UNAUTHORIZED);
    assert_eq!(hub.manifest_of(&new).await.manifest, "manifest-3");
}

#[tokio::test]
async fn a_revoked_code_stops_working() {
    let hub = spawn().await;
    let code = hub.player_code().await;
    hub.store.revoke_player("player").await.expect("revoke");
    assert_eq!(
        hub.status("manifest", &code).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn too_many_requests_are_refused() {
    let hub = spawn().await;
    let mut last = StatusCode::OK;
    for _ in 0..70 {
        last = hub.status("manifest", "wrong").await;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
}

#[test]
fn the_hash_matches_the_one_pack_sync_users_are_given() {
    // Sites that issue codes themselves must hash them the same way.
    assert_eq!(
        hash_code(" abc "),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
