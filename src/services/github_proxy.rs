//! GithubProxy - GitHub API/request proxy with mirror fallback.
//!
//! Makes GitHub requests resilient in regions where github.com / api.github.com are blocked or throttled.
//! Every GitHub request is tried DIRECT first, and on failure re-tried through each capability-matched mirror.
//!
//! API requests (api.github.com) get API mirrors; everything else (github.com downloads, raw, objects)
//! gets download mirrors. The first usable response wins.

use std::time::Duration;

use ureq::Agent;

pub struct GithubProxy {
    http: Agent,
    api_mirrors: Vec<String>,
    download_mirrors: Vec<String>,
}

impl Default for GithubProxy {
    fn default() -> Self {
        Self {
            http: Agent::new(),
            api_mirrors: Vec::new(),
            download_mirrors: Vec::new(),
        }
    }
}

impl GithubProxy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_mirrors(mut self, api_mirrors: Vec<String>, download_mirrors: Vec<String>) -> Self {
        self.api_mirrors = api_mirrors;
        self.download_mirrors = download_mirrors;
        self
    }

    fn is_github(url: &str) -> bool {
        url.starts_with("https://github.com/")
            || url.starts_with("https://api.github.com/")
            || url.starts_with("https://raw.githubusercontent.com/")
            || url.starts_with("https://objects.githubusercontent.com/")
    }

    /// Generate candidate URLs: direct first, then mirror-prefixed variants.
    pub(crate) fn candidates(&self, url: &str) -> Vec<String> {
        let mut candidates = vec![url.to_string()];

        if !Self::is_github(url) {
            return candidates;
        }

        let mirrors = if url.starts_with("https://api.github.com/") {
            &self.api_mirrors
        } else {
            &self.download_mirrors
        };

        for mirror in mirrors {
            candidates.push(format!("{}{}", mirror, url));
        }

        candidates
    }

    /// Send a GET request, trying direct then mirrors. Returns the first successful response.
    pub async fn send_async(&self, url: &str) -> Result<ureq::Response, ureq::Error> {
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
                    // For non-2xx status, create an error with the Response object
                    last_err = Some(ureq::Error::Status(status, res));
                }
                Err(e) => last_err = Some(e),
            }
        }

        Err(last_err.unwrap_or_else(|| ureq::Error::Status(500, ureq::Response::new())))
    }

    /// Download a file, trying direct then mirrors. Streams to disk with progress.
    pub async fn download_async(
        &self,
        url: &str,
        dest_path: &std::path::Path,
        progress: Option<&mut dyn FnMut(f64)>,
    ) -> Result<(), ureq::Error> {
        let mut last: Option<ureq::Error> = None;

        for candidate in self.candidates(url) {
            match self
                .http
                .get(&candidate)
                .set("User-Agent", "LuaToolsGui")
                .call()
            {
                Ok(res) => {
                    if res.status() < 400 {
                        let total = res.header("content-length").and_then(|v| v.parse::<u64>().ok());
                        let mut reader = res.into_reader();
                        let mut written = 0u64;

                        match std::fs::File::create(dest_path) {
                            Ok(mut file) => {
                                let mut buf = [0u8; 81920];
                                loop {
                                    match reader.read(&mut buf) {
                                        Ok(0) => break,
                                        Ok(n) => {
                                            if let Err(_) = file.write_all(&buf[..n]) {
                                                break;
                                            }
                                            written += n as u64;
                                            if let Some(p) = progress.as_mut() {
                                                if let Some(t) = total {
                                                    p((written as f64) / (t as f64));
                                                } else {
                                                    p(0.0);
                                                }
                                            }
                                        }
                                        Err(_) => break,
                                    }
                                }
                                return Ok(());
                            }
                            Err(_) => continue,
                        }
                    }
                }
                Err(e) => last = Some(e),
            }
        }

        Err(last.unwrap_or_else(|| ureq::Error::Transport(ureq::Transport::new())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_github() {
        let p = GithubProxy::new();
        assert!(p.is_github("https://github.com/user/repo"));
        assert!(p.is_github("https://api.github.com/repos/user/repo"));
        assert!(p.is_github("https://raw.githubusercontent.com/user/repo/main/file.txt"));
        assert!(!p.is_github("https://example.com/file"));
        assert!(!p.is_github("https://google.com"));
    }

    #[test]
    fn test_candidates_includes_direct_first() {
        let p = GithubProxy::new();
        let cands = p.candidates("https://github.com/user/repo/archive/refs/heads/main.zip");
        assert_eq!(cands[0], "https://github.com/user/repo/archive/refs/heads/main.zip");
    }

    #[test]
    fn test_non_github_returns_only_self() {
        let p = GithubProxy::new();
        let cands = p.candidates("https://example.com/file");
        assert_eq!(cands, vec!["https://example.com/file"]);
    }
}