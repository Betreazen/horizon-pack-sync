//! The website that hands out signed manifests and download links.

use std::fs::File;
use std::io;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ureq::Agent;
use ureq::http::StatusCode;

use crate::manifest::Signed;

const API_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(45 * 60);
/// A whole game build is about 100 MB, a server export a few hundred.
pub const MAX_DOWNLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;

pub trait Site {
    fn manifest(&self) -> Result<Signed>;
    fn download(&self, sequence: u64, asset: &str, target: &Path) -> Result<()>;
    fn confirm(&self, sequence: u64) -> Result<()>;
}

pub struct HttpSite {
    base: String,
    credential: String,
    api: Agent,
    files: Agent,
}

impl HttpSite {
    pub fn new(base: &str, credential: &str) -> Result<Self> {
        check_base(base)?;
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            credential: format!("Bearer {}", credential.trim()),
            api: agent(API_TIMEOUT, 0),
            files: agent(DOWNLOAD_TIMEOUT, 0),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/pack/{path}", self.base)
    }
}

fn agent(timeout: Duration, redirects: u32) -> Agent {
    Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(redirects)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(timeout))
        .build()
        .into()
}

/// Plain http is allowed only to this computer, for local tests. The host is compared whole, so
/// `http://127.0.0.1.evil.com` and `http://localhost@evil.com` do not pass.
fn is_loopback(address: &str) -> bool {
    let Some(rest) = address.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or_default();
    let host = authority.split(':').next().unwrap_or_default();
    !authority.contains('@') && matches!(host, "127.0.0.1" | "localhost")
}

/// The access code must never travel in the clear.
fn check_base(base: &str) -> Result<()> {
    if base.starts_with("https://") || is_loopback(base) {
        return Ok(());
    }
    bail!("the site address must start with https://")
}

fn refuse(status: StatusCode) -> anyhow::Error {
    match status {
        StatusCode::UNAUTHORIZED => anyhow::anyhow!("the site did not accept the access code"),
        StatusCode::TOO_MANY_REQUESTS => anyhow::anyhow!("too many attempts, try again later"),
        StatusCode::NOT_FOUND => anyhow::anyhow!("the site has no such build yet"),
        other => anyhow::anyhow!("the site answered {other}"),
    }
}

impl Site for HttpSite {
    fn manifest(&self) -> Result<Signed> {
        let mut response = self
            .api
            .get(self.url("manifest"))
            .header("Authorization", &self.credential)
            .call()
            .context("cannot reach the site")?;
        if response.status() != StatusCode::OK {
            return Err(refuse(response.status()));
        }
        response
            .body_mut()
            .read_json()
            .context("the site sent a malformed answer")
    }

    fn download(&self, sequence: u64, asset: &str, target: &Path) -> Result<()> {
        let link = self
            .api
            .get(self.url(&format!("file/{sequence}/{asset}")))
            .header("Authorization", &self.credential)
            .call()
            .context("cannot reach the site")?;
        if !link.status().is_redirection() {
            return Err(refuse(link.status()));
        }
        let location = link
            .headers()
            .get("location")
            .context("the site sent no download link")?
            .to_str()
            .context("the download link is malformed")?;
        check_base(location).context("the site sent an unsafe download link")?;
        // Requested separately so the access code never reaches the file host.
        let mut response = self
            .files
            .get(location)
            .call()
            .context("cannot start the download")?;
        if response.status() != StatusCode::OK {
            bail!("the file host answered {}", response.status());
        }
        let mut reader = response
            .body_mut()
            .with_config()
            .limit(MAX_DOWNLOAD_BYTES)
            .reader();
        let mut file = File::create(target).context("cannot create the download file")?;
        io::copy(&mut reader, &mut file).context("the download was interrupted")?;
        Ok(())
    }

    fn confirm(&self, sequence: u64) -> Result<()> {
        let response = self
            .api
            .post(self.url("confirm"))
            .header("Authorization", &self.credential)
            .send_json(serde_json::json!({ "sequence": sequence }))
            .context("cannot reach the site")?;
        if response.status() != StatusCode::NO_CONTENT {
            return Err(refuse(response.status()));
        }
        Ok(())
    }
}

#[cfg(test)]
pub mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::*;

    /// A site in memory: the files it can serve and what it was told.
    #[derive(Default)]
    pub struct FakeSite {
        pub signed: RefCell<Option<Signed>>,
        pub files: BTreeMap<String, Vec<u8>>,
        pub confirmed: Cell<Option<u64>>,
        pub downloads: Cell<u32>,
        pub is_down: Cell<bool>,
    }

    impl Site for FakeSite {
        fn manifest(&self) -> Result<Signed> {
            if self.is_down.get() {
                bail!("the site is down");
            }
            self.signed.borrow().clone().context("no build")
        }

        fn download(&self, _sequence: u64, asset: &str, target: &Path) -> Result<()> {
            self.downloads.set(self.downloads.get() + 1);
            std::fs::write(target, self.files.get(asset).context("no file")?)?;
            Ok(())
        }

        fn confirm(&self, sequence: u64) -> Result<()> {
            self.confirmed.set(Some(sequence));
            Ok(())
        }
    }

    /// Answers one request per connection and remembers each request's raw text.
    fn spawn_http(respond: fn(&str, &str) -> String) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let (record, address) = (Arc::clone(&seen), base.clone());
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let request = read_request(&mut stream);
                let reply = respond(&request, &address);
                record.lock().expect("lock").push(request);
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        (base, seen)
    }

    /// Reads the head and the whole body: a body can arrive in a later packet than the head, and
    /// answering before it came made the client see a reset connection now and then.
    fn read_request(stream: &mut std::net::TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = stream.read(&mut buffer).unwrap_or(0);
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            let text = String::from_utf8_lossy(&bytes);
            let Some(head_end) = text.find("\r\n\r\n") else {
                continue;
            };
            let length = text[..head_end]
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if bytes.len() >= head_end + 4 + length {
                break;
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn reply(status: &str, extra: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn fake_site(request: &str, base: &str) -> String {
        let line = request.lines().next().unwrap_or_default();
        if line.starts_with("GET /api/pack/manifest") {
            let body = r#"{"manifest":"m","signature":"s"}"#;
            return reply("200 OK", "Content-Type: application/json\r\n", body);
        }
        if line.starts_with("GET /api/pack/file/4/client.gz") {
            return reply(
                "307 Temporary Redirect",
                &format!("Location: {base}/blob\r\n"),
                "",
            );
        }
        if line.starts_with("GET /blob") {
            return reply("200 OK", "", "file bytes");
        }
        if line.starts_with("POST /api/pack/confirm") {
            return reply("204 No Content", "", "");
        }
        reply("401 Unauthorized", "", "")
    }

    fn requests_to(seen: &Mutex<Vec<String>>, path: &str) -> Vec<String> {
        let all = seen.lock().expect("lock");
        all.iter()
            .filter(|request| request.contains(path))
            .cloned()
            .collect()
    }

    #[test]
    fn a_manifest_is_fetched_with_the_access_code() {
        let (base, seen) = spawn_http(fake_site);
        let site = HttpSite::new(&base, " my-code\n").expect("site");
        let signed = site.manifest().expect("manifest");
        assert_eq!(signed.manifest, "m");
        let sent = &requests_to(&seen, "/api/pack/manifest")[0].to_lowercase();
        assert!(sent.contains("authorization: bearer my-code"), "{sent}");
    }

    #[test]
    fn a_download_follows_the_link_without_the_access_code() {
        let (base, seen) = spawn_http(fake_site);
        let dir = tempfile::tempdir().expect("dir");
        let target = dir.path().join("file");
        let site = HttpSite::new(&base, "my-code").expect("site");
        site.download(4, "client.gz", &target).expect("download");
        assert_eq!(std::fs::read(&target).expect("read"), b"file bytes");
        let blob = &requests_to(&seen, "/blob")[0].to_lowercase();
        assert!(!blob.contains("authorization"), "{blob}");
    }

    #[test]
    fn a_confirmation_is_accepted() {
        let (base, _) = spawn_http(fake_site);
        HttpSite::new(&base, "key")
            .expect("site")
            .confirm(4)
            .expect("confirm");
    }

    #[test]
    fn a_refused_code_is_reported_as_such() {
        let (base, _) = spawn_http(|_, _| reply("401 Unauthorized", "", ""));
        let error = HttpSite::new(&base, "bad")
            .expect("site")
            .manifest()
            .expect_err("refused");
        assert!(error.to_string().contains("access code"), "{error}");
    }

    #[test]
    fn an_unencrypted_download_link_is_refused() {
        let (base, _) = spawn_http(|_, _| {
            reply(
                "307 Temporary Redirect",
                "Location: http://evil.example/x\r\n",
                "",
            )
        });
        let dir = tempfile::tempdir().expect("dir");
        let site = HttpSite::new(&base, "code").expect("site");
        assert!(site.download(4, "a", &dir.path().join("f")).is_err());
    }

    #[test]
    fn plain_http_is_refused_except_on_loopback() {
        assert!(check_base("http://example.com").is_err());
        assert!(check_base("https://example.com").is_ok());
        assert!(check_base("http://127.0.0.1:3000").is_ok());
        assert!(check_base("http://localhost/path").is_ok());
    }

    #[test]
    fn a_host_that_only_looks_like_loopback_is_refused() {
        for address in [
            "http://127.0.0.1.evil.com",
            "http://localhost.evil.com",
            "http://localhost@evil.com",
            "http://localhost:80@evil.com",
        ] {
            assert!(check_base(address).is_err(), "{address}");
        }
    }
}
