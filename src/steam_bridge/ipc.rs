//! Inter-process communication between the LuaTools GUI and the Steam
//! integration library running inside the Steam client process.
//!
//! Uses a Unix domain socket for local, low-latency messaging. The integration
//! library listens on the socket inside the Steam process; the GUI connects to
//! it to query status, push configuration changes, and receive events.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::{Deserialize, Serialize};

/// Socket path for the IPC channel.
pub fn ipc_socket_path() -> PathBuf {
    dirs::runtime_dir()
        .unwrap_or_else(|| dirs::cache_dir().unwrap_or_else(|| PathBuf::from("/tmp")))
        .join("luatools_steam_ipc.sock")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IpcMessage {
    /// GUI → Integration: query integration status
    Ping,
    /// Integration → GUI: pong with status
    Pong { active: bool, version: String },
    /// GUI → Integration: set a ticket for an app
    SetTicket {
        app_id: u32,
        steam_id: Option<String>,
        app_ticket: Option<String>,
        e_ticket: Option<String>,
    },
    /// GUI → Integration: remove a ticket for an app
    RemoveTicket { app_id: u32 },
    /// GUI → Integration: enable/disable play-unowned mode
    SetPlayUnowned { enabled: bool },
    /// GUI → Integration: add an app to the play-unowned list
    AddPlayUnownedApp { app_id: u32 },
    /// GUI → Integration: remove an app from the play-unowned list
    RemovePlayUnownedApp { app_id: u32 },
    /// Integration → GUI: async event notification
    Event { kind: String, payload: HashMap<String, String> },
    /// GUI → Integration: set a stat/achievement value for an app
    SetStat { app_id: u32, stat_name: String, stat_value: String },
    /// GUI → Integration: set a legacy third-party CD key for an app
    SetLegacyCdKey { app_id: u32, key: String },
    /// GUI → Integration: set a depot decryption key
    SetDepotKey { depot_id: u32, key: String },
    /// GUI → Integration: fetch a manifest code by GID
    FetchManifestCode { gid: String },
    /// GUI → Integration: fetch a manifest code with app/depot context
    FetchManifestCodeEx { app_id: u32, depot_id: u32, gid: String },
    /// GUI → Integration: set app ID spoof (online fix / 480)
    SetAppIdSpoof { app_id: u32, spoof_id: u32 },
    /// GUI → Integration: remove app ID spoof
    RemoveAppIdSpoof { app_id: u32 },
    /// GUI → Integration: set a manifest pin for enforcement
    SetManifestPin { app_id: u32, depot_id: u32, manifest_id: String, size: u64 },
    /// GUI → Integration: remove a manifest pin
    RemoveManifestPin { app_id: u32, depot_id: u32 },
    /// Integration → GUI: manifest binding status
    ManifestBound { app_id: u32, depot_id: u32, manifest_id: String, active: bool },
}

impl IpcMessage {
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

/// Server side: runs inside the Steam process, listens on the Unix socket.
pub struct IpcServer {
    listener: Option<UnixListener>,
    stop: Arc<AtomicBool>,
    handler: Arc<Mutex<Box<dyn Fn(IpcMessage) -> Option<IpcMessage> + Send + Sync>>>,
}

impl IpcServer {
    pub fn new() -> Self {
        Self {
            listener: None,
            stop: Arc::new(AtomicBool::new(false)),
            handler: Arc::new(Mutex::new(Box::new(|_msg| None))),
        }
    }

    pub fn set_handler<F>(&mut self, handler: F)
    where
        F: Fn(IpcMessage) -> Option<IpcMessage> + Send + Sync + 'static,
    {
        *self.handler.lock().unwrap() = Box::new(handler);
    }

    pub fn start(&mut self) -> Result<(), String> {
        let socket_path = ipc_socket_path();
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path)
            .map_err(|e| format!("Failed to bind IPC socket: {}", e))?;
        self.listener = Some(listener);

        let stop = Arc::clone(&self.stop);
        let handler = Arc::clone(&self.handler);
        let socket_path_clone = socket_path.clone();

        thread::spawn(move || {
            let listener = match std::fs::File::open(&socket_path_clone) {
                Ok(_) => {
                    // Re-bind since we dropped the original listener
                    match UnixListener::bind(&socket_path_clone) {
                        Ok(l) => l,
                        Err(e) => {
                            eprintln!("IPC server rebind failed: {}", e);
                            return;
                        }
                    }
                }
                Err(_) => return,
            };

            for stream in listener.incoming() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                match stream {
                    Ok(mut stream) => {
                        let handler = Arc::clone(&handler);
                        thread::spawn(move || {
                            let mut buf = [0u8; 4096];
                            match stream.read(&mut buf) {
                                Ok(n) if n > 0 => {
                                    let s = String::from_utf8_lossy(&buf[..n]);
                                    if let Ok(msg) = IpcMessage::from_json(s.trim()) {
                                        let response = (handler.lock().unwrap())(msg);
                                        if let Some(resp) = response {
                                            let _ = stream.write_all(resp.to_json().unwrap_or_default().as_bytes());
                                        }
                                    }
                                }
                                _ => {}
                            }
                            let _ = stream.shutdown(Shutdown::Both);
                        });
                    }
                    Err(e) => {
                        eprintln!("IPC accept error: {}", e);
                    }
                }
            }
        });

        Ok(())
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_file(ipc_socket_path());
        self.listener = None;
    }
}

/// Client side: runs in the GUI process, connects to the IPC socket.
pub struct IpcClient {
    socket_path: PathBuf,
}

impl Default for IpcClient {
    fn default() -> Self {
        Self::new()
    }
}

impl IpcClient {
    pub fn new() -> Self {
        Self {
            socket_path: ipc_socket_path(),
        }
    }

    pub fn send(&self, msg: &IpcMessage) -> Result<Option<IpcMessage>, String> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .map_err(|e| format!("Failed to connect to IPC socket: {}", e))?;

        let json = msg.to_json().map_err(|e| format!("Serialization error: {}", e))?;
        stream
            .write_all(json.as_bytes())
            .map_err(|e| format!("Send error: {}", e))?;

        let mut buf = [0u8; 4096];
        let n = stream
            .read(&mut buf)
            .map_err(|e| format!("Receive error: {}", e))?;

        if n == 0 {
            return Ok(None);
        }

        let s = String::from_utf8_lossy(&buf[..n]);
        let reply = IpcMessage::from_json(s.trim())
            .map_err(|e| format!("Deserialization error: {}", e))?;
        Ok(Some(reply))
    }

    pub fn ping(&self) -> Result<bool, String> {
        match self.send(&IpcMessage::Ping)? {
            Some(IpcMessage::Pong { active, .. }) => Ok(active),
            _ => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_serialization_roundtrip() {
        let msg = IpcMessage::Ping;
        let json = msg.to_json().unwrap();
        let parsed = IpcMessage::from_json(&json).unwrap();
        assert!(matches!(parsed, IpcMessage::Ping));
    }

    #[test]
    fn pong_message_roundtrip() {
        let mut map = HashMap::new();
        map.insert("key".into(), "val".into());
        let msg = IpcMessage::Event {
            kind: "test".into(),
            payload: map,
        };
        let json = msg.to_json().unwrap();
        let parsed = IpcMessage::from_json(&json).unwrap();
        assert!(matches!(parsed, IpcMessage::Event { kind, .. } if kind == "test"));
    }

    #[test]
    fn stat_message_roundtrip() {
        let msg = IpcMessage::SetStat {
            app_id: 1245620,
            stat_name: "steamid".into(),
            stat_value: "76561198028121353".into(),
        };
        let json = msg.to_json().unwrap();
        let parsed = IpcMessage::from_json(&json).unwrap();
        assert!(matches!(parsed, IpcMessage::SetStat { app_id, .. } if app_id == 1245620));
    }

    #[test]
    fn legacy_cdkey_message_roundtrip() {
        let msg = IpcMessage::SetLegacyCdKey {
            app_id: 1245620,
            key: "ABCD-EFGH-IJKL".into(),
        };
        let json = msg.to_json().unwrap();
        let parsed = IpcMessage::from_json(&json).unwrap();
        assert!(matches!(parsed, IpcMessage::SetLegacyCdKey { app_id, .. } if app_id == 1245620));
    }

    #[test]
    fn depot_key_message_roundtrip() {
        let msg = IpcMessage::SetDepotKey {
            depot_id: 1245621,
            key: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
        };
        let json = msg.to_json().unwrap();
        let parsed = IpcMessage::from_json(&json).unwrap();
        assert!(matches!(parsed, IpcMessage::SetDepotKey { depot_id, .. } if depot_id == 1245621));
    }

    #[test]
    fn manifest_code_ex_message_roundtrip() {
        let msg = IpcMessage::FetchManifestCodeEx {
            app_id: 1245620,
            depot_id: 1245621,
            gid: "abc123".into(),
        };
        let json = msg.to_json().unwrap();
        let parsed = IpcMessage::from_json(&json).unwrap();
        assert!(matches!(
            parsed,
            IpcMessage::FetchManifestCodeEx { app_id, depot_id, .. }
                if app_id == 1245620 && depot_id == 1245621
        ));
    }

    #[test]
    fn app_id_spoof_message_roundtrip() {
        let msg = IpcMessage::SetAppIdSpoof {
            app_id: 1245620,
            spoof_id: 480,
        };
        let json = msg.to_json().unwrap();
        let parsed = IpcMessage::from_json(&json).unwrap();
        assert!(matches!(
            parsed,
            IpcMessage::SetAppIdSpoof { app_id, spoof_id }
                if app_id == 1245620 && spoof_id == 480
        ));
    }

    #[test]
    fn manifest_pin_message_roundtrip() {
        let msg = IpcMessage::SetManifestPin {
            app_id: 1245620,
            depot_id: 1245621,
            manifest_id: "abc123def456".into(),
            size: 1024000,
        };
        let json = msg.to_json().unwrap();
        let parsed = IpcMessage::from_json(&json).unwrap();
        assert!(matches!(
            parsed,
            IpcMessage::SetManifestPin { app_id, depot_id, manifest_id, size }
                if app_id == 1245620 && depot_id == 1245621 && manifest_id == "abc123def456" && size == 1024000
        ));
    }

    #[test]
    fn remove_manifest_pin_message_roundtrip() {
        let msg = IpcMessage::RemoveManifestPin { app_id: 1245620, depot_id: 1245621 };
        let json = msg.to_json().unwrap();
        let parsed = IpcMessage::from_json(&json).unwrap();
        assert!(matches!(
            parsed,
            IpcMessage::RemoveManifestPin { app_id, depot_id }
                if app_id == 1245620 && depot_id == 1245621
        ));
    }

    #[test]
    fn manifest_bound_message_roundtrip() {
        let msg = IpcMessage::ManifestBound { app_id: 1245620, depot_id: 1245621, manifest_id: "abc123".into(), active: true };
        let json = msg.to_json().unwrap();
        let parsed = IpcMessage::from_json(&json).unwrap();
        assert!(matches!(
            parsed,
            IpcMessage::ManifestBound { app_id, depot_id, manifest_id, active }
                if app_id == 1245620 && depot_id == 1245621 && manifest_id == "abc123" && active == true
        ));
    }
}
