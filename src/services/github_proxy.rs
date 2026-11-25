//! GithubProxy - GitHub API/request proxy with mirror fallback.
//!
//! Makes GitHub requests resilient in regions where github.com / api.github.com are blocked or throttled.
//! Every GitHub request is tried DIRECT first, and on failure re-tried through each capability-matched mirror.
//!
//! API requests (api.github.com) get API mirrors; everything else (github.com downloads, raw, objects)
//! gets download mirrors. The first usable response wins.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ureq::Agent;

static PROXY_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

pub struct GithubProxy {
    http: Agent,
    timeout: Duration,
    pub(crate) api_mirrors: Vec<String>,
    pub(crate) download_mirrors: Vec<String>,
}

impl Default for GithubProxy {
    fn default() -> Self {
        Self {
            http: Self::build_agent(DEFAULT_TIMEOUT),
            timeout: DEFAULT_TIMEOUT,
            api_mirrors: vec![
                "https://ghfast.top/".into(),
                "https://gh-proxy.com/".into(),
            ],
            download_mirrors: vec![
                "https://ghfast.top/".into(),
                "https://gh-proxy.com/".into(),
            ],
        }
    }
}

impl GithubProxy {
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

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self.http = Self::build_agent(timeout);
        self
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn with_mirrors(mut self, api_mirrors: Vec<String>, download_mirrors: Vec<String>) -> Self {
        self.api_mirrors = api_mirrors;
        self.download_mirrors = download_mirrors;
        self
    }

    pub fn api_mirrors(&self) -> &[String] {
        &self.api_mirrors
    }

    pub fn download_mirrors(&self) -> &[String] {
        &self.download_mirrors
    }

    pub fn is_github(url: &str) -> bool {
        let low = url.trim().to_ascii_lowercase();
        low.starts_with("https://github.com/")
            || low.starts_with("https://api.github.com/")
            || low.starts_with("https://raw.githubusercontent.com/")
            || low.starts_with("https://objects.githubusercontent.com/")
            || low.starts_with("https://codeload.github.com/")
            || low.starts_with("https://release-assets.githubusercontent.com/")
    }

    /// Generate candidate URLs: direct first, then mirror-prefixed variants.
    pub(crate) fn candidates(&self, url: &str) -> Vec<String> {
        let trimmed_url = url.trim();
        let mut candidates = vec![trimmed_url.to_string()];

        if !Self::is_github(trimmed_url) {
            return candidates;
        }

        let mirrors = if trimmed_url
            .to_ascii_lowercase()
            .starts_with("https://api.github.com/")
        {
            &self.api_mirrors
        } else {
            &self.download_mirrors
        };

        let mut seen = HashSet::new();
        seen.insert(trimmed_url.to_string());

        for mirror in mirrors {
            let m = mirror.trim();
            if m.is_empty() {
                continue;
            }
            // Strip any trailing full URL prefix from mirror (e.g. "https://proxy/https://api.github.com")
            // to avoid duplicating host prefixes when joined with full trimmed_url.
            let m_clean = if let Some(idx) = m.find("/https://") {
                &m[..=idx]
            } else if let Some(idx) = m.find("/http://") {
                &m[..=idx]
            } else {
                m
            };
            let joined = if m_clean.ends_with('/') || m_clean.ends_with('=') || m_clean.ends_with('?') {
                format!("{}{}", m_clean, trimmed_url)
            } else {
                format!("{}/{}", m_clean, trimmed_url)
            };
            if seen.insert(joined.clone()) {
                candidates.push(joined);
            }
        }

        candidates
    }

    /// Send a GET request, trying direct then mirrors. Returns the first successful response.
    pub async fn send_async(&self, url: &str) -> Result<ureq::Response, Box<ureq::Error>> {
        let mut last_err: Option<ureq::Error> = None;

        for candidate in self.candidates(url) {
            match self
                .http
                .get(&candidate)
                .set("User-Agent", "LuaToolsGui")
                .set("Accept", "application/vnd.github+json")
                .call()
            {
                Ok(res) if res.status() < 400 => return Ok(res),
                Ok(res) => {
                    let status = res.status();
                    last_err = Some(ureq::Error::Status(status, res));
                }
                Err(e) => last_err = Some(e),
            }
        }

        Err(Box::new(last_err.unwrap_or_else(|| {
            ureq::Error::from(io::Error::other("All GitHub mirrors failed"))
        })))
    }

    /// Download a file synchronously, trying direct then mirrors. Streams atomically to disk with progress.
    pub fn download_sync(
        &self,
        url: &str,
        dest_path: &std::path::Path,
        mut progress: Option<&mut dyn FnMut(f64)>,
    ) -> Result<(), Box<ureq::Error>> {
        if let Some(parent) = dest_path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                return Err(Box::new(ureq::Error::from(e)));
            }
        }

        let mut last: Option<ureq::Error> = None;
        let base_name = dest_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("download");

        for candidate in self.candidates(url) {
            match self
                .http
                .get(&candidate)
                .set("User-Agent", "LuaToolsGui")
                .call()
            {
                Ok(res) if res.status() < 400 => {
                    let total = res
                        .header("content-length")
                        .and_then(|v| v.parse::<u64>().ok())
                        .filter(|&t| t > 0);
                    let mut reader = res.into_reader();
                    let mut written = 0u64;

                    let seq = PROXY_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
                    let tmp_path = dest_path.with_file_name(format!(
                        ".{}.part.{}.{}",
                        base_name,
                        std::process::id(),
                        seq
                    ));

                    match fs::File::create(&tmp_path) {
                        Ok(mut file) => {
                            let mut buf = [0u8; 81920];
                            let mut stream_err: Option<io::Error> = None;
                            loop {
                                match reader.read(&mut buf) {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        if let Err(e) = file.write_all(&buf[..n]) {
                                            stream_err = Some(e);
                                            break;
                                        }
                                        written += n as u64;
                                        if let Some(p) = progress.as_mut() {
                                            if let Some(t) = total {
                                                p(((written as f64) / (t as f64)).clamp(0.0, 1.0));
                                            } else {
                                                p(0.0);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        stream_err = Some(e);
                                        break;
                                    }
                                }
                            }
                            if stream_err.is_none() {
                                if let Err(e) = file.flush() {
                                    stream_err = Some(e);
                                }
                            }
                            drop(file);
                            if let Some(err) = stream_err {
                                let _ = fs::remove_file(&tmp_path);
                                last = Some(ureq::Error::from(err));
                                continue;
                            }
                            if let Err(e) = fs::rename(&tmp_path, dest_path) {
                                let _ = fs::remove_file(&tmp_path);
                                last = Some(ureq::Error::from(e));
                                continue;
                            }
                            return Ok(());
                        }
                        Err(e) => {
                            last = Some(ureq::Error::from(e));
                            continue;
                        }
                    }
                }
                Ok(res) => last = Some(ureq::Error::Status(res.status(), res)),
                Err(e) => last = Some(e),
            }
        }

        Err(Box::new(last.unwrap_or_else(|| {
            ureq::Error::from(io::Error::other("All GitHub mirrors failed"))
        })))
    }

    /// Download a file, trying direct then mirrors. Streams atomically to disk with progress.
    pub async fn download_async(
        &self,
        url: &str,
        dest_path: &std::path::Path,
        progress: Option<&mut dyn FnMut(f64)>,
    ) -> Result<(), Box<ureq::Error>> {
        self.download_sync(url, dest_path, progress)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_github() {
        assert!(GithubProxy::is_github("https://github.com/user/repo"));
        assert!(GithubProxy::is_github(
            "https://api.github.com/repos/user/repo"
        ));
        assert!(GithubProxy::is_github(
            "https://raw.githubusercontent.com/user/repo/main/file.txt"
        ));
        assert!(GithubProxy::is_github(
            "https://codeload.github.com/user/repo/zip/main"
        ));
        assert!(GithubProxy::is_github(
            "https://release-assets.githubusercontent.com/github-production-release-asset/1"
        ));
        assert!(!GithubProxy::is_github("https://example.com/file"));
        assert!(!GithubProxy::is_github("https://google.com"));
    }

    #[test]
    fn test_candidates_includes_direct_first_and_normalizes_mirrors() {
        let p = GithubProxy::new().with_mirrors(
            vec![
                "https://api-mirror.example".into(),
                "https://ghfast.top/https://api.github.com".into(),
            ],
            vec![
                "  ".into(),
                "https://ghproxy.example".into(),
                "https://ghproxy.example/".into(),
                "https://proxy2.example/?url=".into(),
            ],
        );
        let cands = p.candidates("https://github.com/user/repo/archive/refs/heads/main.zip");
        assert_eq!(
            cands,
            vec![
                "https://github.com/user/repo/archive/refs/heads/main.zip",
                "https://ghproxy.example/https://github.com/user/repo/archive/refs/heads/main.zip",
                "https://proxy2.example/?url=https://github.com/user/repo/archive/refs/heads/main.zip",
            ]
        );

        let api_cands = p.candidates("https://api.github.com/repos/user/repo");
        assert_eq!(
            api_cands,
            vec![
                "https://api.github.com/repos/user/repo",
                "https://api-mirror.example/https://api.github.com/repos/user/repo",
                "https://ghfast.top/https://api.github.com/repos/user/repo",
            ]
        );
    }

    #[test]
    fn test_non_github_returns_only_self() {
        let p = GithubProxy::new();
        let cands = p.candidates("https://example.com/file");
        assert_eq!(cands, vec!["https://example.com/file"]);
    }

    #[test]
    fn test_with_timeout_updates_configured_timeout() {
        let default_proxy = GithubProxy::new();
        assert_eq!(default_proxy.timeout(), Duration::from_secs(30));

        let custom_proxy = GithubProxy::new().with_timeout(Duration::from_secs(90));
        assert_eq!(custom_proxy.timeout(), Duration::from_secs(90));
    }
}
