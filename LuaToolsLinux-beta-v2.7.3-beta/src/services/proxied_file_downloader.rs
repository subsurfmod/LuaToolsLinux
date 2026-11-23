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

pub struct ProxiedFileDownloader {
    proxy: GithubProxy,
    http: Agent,
}

impl Default for ProxiedFileDownloader {
    fn default() -> Self {
        let http = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .build();
        Self {
            proxy: GithubProxy::new(),
            http,
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
    ) -> Result<(), Box<ureq::Error>> {
        self.proxy.download_async(url, target_file, progress).await
    }

    /// Download raw bytes into memory with mirror fallback (binary-safe).
    pub async fn download_bytes(&self, url: &str) -> Result<Vec<u8>, Box<ureq::Error>> {
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
                    match res.into_reader().read_to_end(&mut buf) {
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

    /// Download a string (text content) with mirror fallback.
    pub async fn download_string(&self, url: &str) -> Result<String, Box<ureq::Error>> {
        let mut last: Option<ureq::Error> = None;

        for candidate in self.proxy.candidates(url) {
            match self
                .http
                .get(&candidate)
                .set("User-Agent", "LuaToolsGui")
                .call()
            {
                Ok(res) if res.status() < 400 => {
                    return res
                        .into_string()
                        .map_err(|e| Box::new(ureq::Error::from(e)));
                }
                Ok(res) => last = Some(ureq::Error::Status(res.status(), res)),
                Err(e) => last = Some(e),
            }
        }

        Err(Box::new(last.unwrap_or_else(|| {
            ureq::Error::from(io::Error::other("All download mirrors failed"))
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_downloader_creation() {
        let downloader = ProxiedFileDownloader::new();
        assert!(downloader.proxy.api_mirrors().is_empty());
        assert!(downloader.proxy.download_mirrors().is_empty());
    }
}
