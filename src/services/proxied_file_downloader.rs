//! ProxiedFileDownloader - File downloader with GitHub mirror fallback.
//!
//! Downloads files with automatic mirror fallback for GitHub URLs.
//! Tries the direct URL first, then falls through mirror alternatives.
//! Used by the update service for resilient downloads in blocked regions.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use ureq::Agent;

use crate::services::github_proxy::GithubProxy;

pub struct ProxiedFileDownloader {
    proxy: GithubProxy,
    http: Agent,
}

impl Default for ProxiedFileDownloader {
    fn default() -> Self {
        Self {
            proxy: GithubProxy::new(),
            http: Agent::new(),
        }
    }
}

impl ProxiedFileDownloader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_proxy(mut self, proxy: GithubProxy) -> Self {
        self.proxy = proxy;
        self
    }

    /// Download a file to disk with progress reporting.
    /// Tries direct URL first, then mirror fallbacks.
    pub async fn download_file(
        &self,
        url: &str,
        target_file: &Path,
        progress: Option<&mut dyn FnMut(f64)>,
    ) -> Result<(), ureq::Error> {
        self.proxy.download_async(url, target_file, progress).await
    }

    /// Download bytes into memory with mirror fallback.
    pub async fn download_bytes(&self, url: &str) -> Result<Vec<u8>, ureq::Error> {
        let mut last: Option<ureq::Error> = None;

        for candidate in self.proxy.candidates(url) {
            match self.http.get(&candidate).set("User-Agent", "LuaToolsGui").call() {
                Ok(res) if res.status() < 400 => {
                    return res.into_string().map_err(|e| e.into())?.into_bytes();
                }
                Ok(res) => last = Some(ureq::Error::Status(res.status(), res)),
                Err(e) => last = Some(e),
            }
        }

        Err(last
            .unwrap_or_else(|| ureq::Error::Transport(ureq::Transport::new())))
    }

    /// Download a string (text content) with mirror fallback.
    pub async fn download_string(&self, url: &str) -> Result<String, ureq::Error> {
        let mut last: Option<ureq::Error> = None;

        for candidate in self.proxy.candidates(url) {
            match self.http.get(&candidate).set("User-Agent", "LuaToolsGui").call() {
                Ok(res) if res.status() < 400 => {
                    return res.into_string().map_err(|e| e.into());
                }
                Ok(res) => last = Some(ureq::Error::Status(res.status(), res)),
                Err(e) => last = Some(e),
            }
        }

        Err(last
            .unwrap_or_else(|| ureq::Error::Transport(ureq::Transport::new())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_downloader_creation() {
        let downloader = ProxiedFileDownloader::new();
        assert!(downloader.proxy.api_mirrors.is_empty());
        assert!(downloader.proxy.download_mirrors.is_empty());
    }
}