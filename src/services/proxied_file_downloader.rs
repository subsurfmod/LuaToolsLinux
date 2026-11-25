//! ProxiedFileDownloader - File downloader with GitHub mirror fallback.
//!
//! Downloads files with automatic mirror fallback for GitHub URLs.
//! Tries the direct URL first, then falls through mirror alternatives.
//! Used by the update service for resilient downloads in blocked regions.

use std::io::{self, Read};
use std::path::Path;
use std::time::Duration;

use ureq::Agent;

use crate::services::github_proxy::GithubProxy;

const MAX_IN_MEMORY_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

pub struct ProxiedFileDownloader {
    proxy: GithubProxy,
    http: Agent,
    timeout_override: Option<Duration>,
}

impl Default for ProxiedFileDownloader {
    fn default() -> Self {
        Self {
            proxy: GithubProxy::new(),
            http: Self::build_agent(DEFAULT_TIMEOUT),
            timeout_override: None,
        }
    }
}

impl ProxiedFileDownloader {
    fn build_agent(timeout: Duration) -> Agent {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(timeout)
            .timeout(timeout)
            .build()
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_proxy(mut self, proxy: GithubProxy) -> Self {
        if let Some(t) = self.timeout_override {
            self.proxy = proxy.with_timeout(t);
        } else {
            self.http = Self::build_agent(proxy.timeout());
            self.proxy = proxy;
        }
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout_override = Some(timeout);
        self.http = Self::build_agent(timeout);
        self.proxy = self.proxy.with_timeout(timeout);
        self
    }

    pub fn timeout(&self) -> Duration {
        self.proxy.timeout()
    }

    /// Download a file to disk synchronously with progress reporting and mirror fallback.
    pub fn download_file_sync(
        &self,
        url: &str,
        target_file: &Path,
        progress: Option<&mut dyn FnMut(f64)>,
    ) -> Result<(), Box<ureq::Error>> {
        self.proxy.download_sync(url, target_file, progress)
    }

    /// Download raw bytes into memory synchronously with mirror fallback (binary-safe, capped at 64 MiB).
    pub fn download_bytes_sync(&self, url: &str) -> Result<Vec<u8>, Box<ureq::Error>> {
        let mut last: Option<ureq::Error> = None;

        for candidate in self.proxy.candidates(url) {
            match self
                .http
                .get(&candidate)
                .set("User-Agent", "LuaToolsGui")
                .call()
            {
                Ok(res) if res.status() < 400 => {
                    let mut buf = Vec::new();
                    match res
                        .into_reader()
                        .take(MAX_IN_MEMORY_BYTES + 1)
                        .read_to_end(&mut buf)
                    {
                        Ok(_) if (buf.len() as u64) > MAX_IN_MEMORY_BYTES => {
                            last = Some(ureq::Error::from(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "Response exceeded 64 MiB in-memory download limit",
                            )));
                        }
                        Ok(_) => return Ok(buf),
                        Err(e) => last = Some(ureq::Error::from(e)),
                    }
                }
                Ok(res) => last = Some(ureq::Error::Status(res.status(), res)),
                Err(e) => last = Some(e),
            }
        }

        Err(Box::new(last.unwrap_or_else(|| {
            ureq::Error::from(io::Error::other("All download mirrors failed"))
        })))
    }

    /// Download a string (text content) synchronously with mirror fallback.
    pub fn download_string_sync(&self, url: &str) -> Result<String, Box<ureq::Error>> {
        let mut last: Option<ureq::Error> = None;

        for candidate in self.proxy.candidates(url) {
            match self
                .http
                .get(&candidate)
                .set("User-Agent", "LuaToolsGui")
                .call()
            {
                Ok(res) if res.status() < 400 => match res.into_string() {
                    Ok(text) => return Ok(text),
                    Err(e) => last = Some(ureq::Error::from(e)),
                },
                Ok(res) => last = Some(ureq::Error::Status(res.status(), res)),
                Err(e) => last = Some(e),
            }
        }

        Err(Box::new(last.unwrap_or_else(|| {
            ureq::Error::from(io::Error::other("All download mirrors failed"))
        })))
    }

    /// Download a file to disk with progress reporting.
    /// Tries direct URL first, then mirror fallbacks.
    pub async fn download_file(
        &self,
        url: &str,
        target_file: &Path,
        progress: Option<&mut dyn FnMut(f64)>,
    ) -> Result<(), Box<ureq::Error>> {
        self.download_file_sync(url, target_file, progress)
    }

    /// Download raw bytes into memory with mirror fallback (binary-safe, capped at 64 MiB).
    pub async fn download_bytes(&self, url: &str) -> Result<Vec<u8>, Box<ureq::Error>> {
        self.download_bytes_sync(url)
    }

    /// Download a string (text content) with mirror fallback.
    pub async fn download_string(&self, url: &str) -> Result<String, Box<ureq::Error>> {
        self.download_string_sync(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

    fn block_on_ready<F: std::future::Future>(fut: F) -> F::Output {
        fn noop(_: *const ()) {}
        fn clone(p: *const ()) -> RawWaker {
            RawWaker::new(p, &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
        let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
        let mut cx = Context::from_waker(&waker);
        let mut fut = std::pin::pin!(fut);
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("expected synchronous future to complete"),
        }
    }

    #[test]
    fn test_downloader_creation() {
        let downloader = ProxiedFileDownloader::new();
        assert_eq!(downloader.proxy.download_mirrors().len(), 2);
        assert_eq!(downloader.timeout(), Duration::from_secs(30));

        let long_dl = ProxiedFileDownloader::new().with_timeout(Duration::from_secs(90));
        assert_eq!(long_dl.timeout(), Duration::from_secs(90));
    }

    #[test]
    fn test_download_bytes_string_and_file_over_loopback() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let handle = std::thread::spawn(move || {
            for _ in 0..3 {
                let req = server.recv().unwrap();
                match req.url() {
                    "/bin" => {
                        let _ = req.respond(tiny_http::Response::from_data(vec![0u8, 1, 2, 255]));
                    }
                    "/txt" => {
                        let _ = req.respond(tiny_http::Response::from_string("hello-mirror"));
                    }
                    _ => {
                        let _ = req.respond(tiny_http::Response::from_data(b"file-body".to_vec()));
                    }
                }
            }
        });

        let downloader = ProxiedFileDownloader::new();
        let bytes =
            block_on_ready(downloader.download_bytes(&format!("http://{}/bin", addr))).unwrap();
        assert_eq!(bytes, vec![0u8, 1, 2, 255]);

        let text =
            block_on_ready(downloader.download_string(&format!("http://{}/txt", addr))).unwrap();
        assert_eq!(text, "hello-mirror");

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sub/downloaded.bin");
        let mut last_progress = 0.0;
        let mut cb = |p: f64| last_progress = p;
        block_on_ready(downloader.download_file(
            &format!("http://{}/file", addr),
            &target,
            Some(&mut cb),
        ))
        .unwrap();
        handle.join().unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"file-body");
        assert!((last_progress - 1.0).abs() < 0.01);
    }
}
