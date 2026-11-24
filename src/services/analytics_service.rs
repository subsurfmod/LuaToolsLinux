//! AnalyticsService - Anonymous app-launch telemetry via Umami.
//!
//! Best-effort fire-and-forget: never throws, never blocks startup.
//! Posts a pageview event to Umami's /api/send endpoint with app launch info.

use std::time::Duration;

use ureq::Agent;

use crate::VERSION;

pub struct AnalyticsService {
    http: Agent,
    umami_host: String,
    website_id: String,
}

impl Default for AnalyticsService {
    fn default() -> Self {
        Self::with_config("https://api.umami.is", "luatools-linux")
    }
}

impl AnalyticsService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(umami_host: impl Into<String>, website_id: impl Into<String>) -> Self {
        let http = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(2))
            .timeout(Duration::from_secs(3))
            .build();
        let host = umami_host.into().trim().trim_end_matches('/').to_string();
        Self {
            http,
            umami_host: host,
            website_id: website_id.into().trim().to_string(),
        }
    }

    fn build_launch_payload(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "event",
            "payload": {
                "website": self.website_id,
                "hostname": "luatools-linux",
                "url": format!("/launch/{}", VERSION),
                "title": "App Launch",
            },
        })
    }

    /// Track an app launch. Best-effort: never throws.
    pub fn track_app_launch(&self) {
        if self.umami_host.is_empty() || self.website_id.is_empty() {
            return;
        }
        let body = self.build_launch_payload();

        let _ = self
            .http
            .post(&format!("{}/api/send", self.umami_host))
            .set(
                "User-Agent",
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36",
            )
            .send_json(body);
    }

    /// Track an app launch strictly gated by user consent (LT-46).
    /// If user has not consented or telemetry is disabled, returns false and does nothing.
    pub fn track_app_launch_gated(&self, consent_seen: bool, telemetry_enabled: bool) -> bool {
        if !consent_seen || !telemetry_enabled {
            return false;
        }
        self.track_app_launch();
        true
    }

    /// Track a custom event strictly gated by user consent (LT-46).
    pub fn track_event_gated(
        &self,
        event_name: &str,
        properties: Option<serde_json::Value>,
        consent_seen: bool,
        telemetry_enabled: bool,
    ) -> bool {
        if !consent_seen || !telemetry_enabled {
            return false;
        }
        if self.umami_host.is_empty() || self.website_id.is_empty() {
            return false;
        }
        if !self.umami_host.starts_with("http://") && !self.umami_host.starts_with("https://") {
            return false;
        }

        let clean_name = scrub_pii(event_name);
        let mut payload = serde_json::json!({
            "website": self.website_id,
            "hostname": "luatools-linux",
            "url": format!("/event/{}", clean_name),
            "title": clean_name,
        });
        if let Some(props) = properties {
            payload["data"] = props;
        }

        let body = serde_json::json!({
            "type": "event",
            "payload": payload,
        });

        let _ = self
            .http
            .post(&format!("{}/api/send", self.umami_host))
            .set(
                "User-Agent",
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36",
            )
            .send_json(body);
        true
    }
}

fn scrub_pii(input: &str) -> String {
    let mut out = input.to_string();
    if let Some(home) = dirs::home_dir().and_then(|p| p.to_str().map(ToString::to_string)) {
        out = out.replace(&home, "~");
    }
    out
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
    fn test_launch_payload_uses_website_id_not_host() {
        let svc = AnalyticsService::with_config("https://api.umami.is/", "site-uuid-123");
        assert_eq!(svc.umami_host, "https://api.umami.is");
        let payload = svc.build_launch_payload();
        assert_eq!(payload["type"], "event");
        assert_eq!(payload["payload"]["website"], "site-uuid-123");
        assert_eq!(payload["payload"]["hostname"], "luatools-linux");
        assert_eq!(payload["payload"]["url"], format!("/launch/{}", VERSION));
    }

    #[test]
    fn test_track_app_launch_does_not_panic() {
        // Use a closed loopback port so the test never hits external networks.
        let svc = AnalyticsService::with_config("http://127.0.0.1:1", "luatools-linux");
        svc.track_app_launch();
    }

    #[test]
    fn test_track_app_launch_posts_to_endpoint() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let handle = std::thread::spawn(move || {
            let mut req = server.recv().unwrap();
            let url = req.url().to_string();
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body).unwrap();
            let _ = req.respond(tiny_http::Response::from_string("ok"));
            (url, body)
        });

        let svc = AnalyticsService::with_config(format!("http://{}", addr), "my-site-id");
        svc.track_app_launch();

        let (url, body) = handle.join().unwrap();
        assert_eq!(url, "/api/send");
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["payload"]["website"], "my-site-id");
        assert_eq!(parsed["payload"]["hostname"], "luatools-linux");
    }

    #[test]
    fn test_telemetry_consent_gating() {
        let svc = AnalyticsService::with_config("http://127.0.0.1:1", "my-site-id");
        // No consent -> returns false immediately without network call
        assert!(!svc.track_app_launch_gated(false, true));
        assert!(!svc.track_app_launch_gated(true, false));
        assert!(!svc.track_app_launch_gated(false, false));
        assert!(!svc.track_event_gated("test_event", None, false, true));
    }
}
