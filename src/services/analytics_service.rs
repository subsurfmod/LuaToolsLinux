//! AnalyticsService - Anonymous app-launch telemetry via Umami.
//!
//! Best-effort fire-and-forget: never throws, never blocks startup.
//! Posts a pageview event to Umami's /api/send endpoint with app launch info.

use std::time::Duration;

use serde::Serialize;
use ureq::Agent;

use crate::VERSION;

pub struct AnalyticsService {
    http: Agent,
    umami_host: String,
    website_id: String,
}

impl Default for AnalyticsService {
    fn default() -> Self {
        Self {
            http: Agent::new(),
            umami_host: "https://api.umami.is".to_string(),
            website_id: "luatools-linux".to_string(),
        }
    }
}

impl AnalyticsService {
    pub fn new() -> Self {
        Self::default()
    }

    /// Track an app launch. Best-effort: never throws.
    pub fn track_app_launch(&self) {
        let version = env!("CARGO_PKG_VERSION");
        let body = serde_json::json!({
            "type": "event",
            "payload": {
                "website": self.umami_host,
                "hostname": self.website_id,
                "url": format!("/launch/{}", version),
                "title": "App Launch",
            },
        });

        let _ = self
            .http
            .post(&format!("{}/api/send", self.umami_host))
            .set(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
            )
            .send_json(body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analytics_creation() {
        let svc = AnalyticsService::new();
        assert_eq!(svc.umami_host, "https://api.umami.is");
        assert_eq!(svc.website_id, "luatools-linux");
    }

    #[test]
    fn test_track_app_launch_does_not_panic() {
        let svc = AnalyticsService::new();
        // Should not panic even if network is unreachable
        svc.track_app_launch();
    }
}