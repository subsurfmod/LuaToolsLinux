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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub type IpcHandlerFn = dyn Fn(IpcMessage) -> Option<IpcMessage> + Send + Sync;

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
    Event {
        kind: String,
        payload: HashMap<String, String>,
    },
    /// GUI → Integration: set a stat/achievement value for an app
    SetStat {
        app_id: u32,
        stat_name: String,
        stat_value: String,
    },
    /// GUI → Integration: remove a stat override for an app
    RemoveStat { app_id: u32, stat_name: String },
    /// GUI → Integration: set a legacy third-party CD key for an app
    SetLegacyCdKey { app_id: u32, key: String },
    /// GUI → Integration: remove a legacy CD key for an app
    RemoveLegacyCdKey { app_id: u32 },
    /// GUI → Integration: set a depot key
    SetDepotKey { depot_id: u32, key: String },
    /// GUI → Integration: remove a depot key
    RemoveDepotKey { depot_id: u32 },
    /// GUI → Integration: fetch a manifest code by GID
    FetchManifestCode { gid: String },
    /// GUI → Integration: fetch a manifest code with app/depot context
    FetchManifestCodeEx {
        app_id: u32,
        depot_id: u32,
        gid: String,
    },
    /// GUI → Integration: set app ID spoof (online fix / 480)
    SetAppIdSpoof { app_id: u32, spoof_id: u32 },
    /// GUI → Integration: remove app ID spoof
    RemoveAppIdSpoof { app_id: u32 },
    /// GUI → Integration: set a manifest pin for enforcement
    SetManifestPin {
        app_id: u32,
        depot_id: u32,
        manifest_id: String,
        size: u64,
    },
    /// GUI → Integration: remove a manifest pin
    RemoveManifestPin { app_id: u32, depot_id: u32 },
    /// Integration → GUI: manifest binding status
    ManifestBound {
        app_id: u32,
        depot_id: u32,
        manifest_id: String,
        active: bool,
    },
    /// Integration → GUI: manifest code response
    ManifestCodeResponse { gid: String, code: String },
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
    socket_path: PathBuf,
    listener: Option<UnixListener>,
    stop: Arc<AtomicBool>,
    handler: Arc<Mutex<Box<IpcHandlerFn>>>,
}

impl Default for IpcServer {
    fn default() -> Self {
        Self::new()
    }
}

impl IpcServer {
    pub fn new() -> Self {
        Self::with_socket_path(ipc_socket_path())
    }

    pub fn with_socket_path(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            listener: None,
            stop: Arc::new(AtomicBool::new(false)),
            handler: Arc::new(Mutex::new(Box::new(|_msg| None))),
        }
    }

    pub fn set_handler<F>(&mut self, handler: F)
    where
        F: Fn(IpcMessage) -> Option<IpcMessage> + Send + Sync + 'static,
    {
        *self.handler.lock().unwrap_or_else(|e| e.into_inner()) = Box::new(handler);
    }

    pub fn start(&mut self) -> Result<(), String> {
        if let Some(parent) = self.socket_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::remove_file(&self.socket_path);
        let listener = UnixListener::bind(&self.socket_path).map_err(|e| {
            format!(
                "Failed to bind IPC socket {}: {}",
                self.socket_path.display(),
                e
            )
        })?;
        let thread_listener = listener
            .try_clone()
            .map_err(|e| format!("Failed to clone IPC listener: {}", e))?;
        self.stop.store(false, Ordering::Relaxed);
        self.listener = Some(listener);

        let stop = Arc::clone(&self.stop);
        let handler = Arc::clone(&self.handler);

        thread::spawn(move || {
            for stream in thread_listener.incoming() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                match stream {
                    Ok(mut stream) => {
                        let handler = Arc::clone(&handler);
                        thread::spawn(move || {
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
                            let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
                            let mut raw = String::new();
                            if stream.read_to_string(&mut raw).is_ok() && !raw.trim().is_empty() {
                                if let Ok(msg) = IpcMessage::from_json(raw.trim()) {
                                    let response = std::panic::catch_unwind(
                                        std::panic::AssertUnwindSafe(|| {
                                            let guard =
                                                handler.lock().unwrap_or_else(|e| e.into_inner());
                                            (guard)(msg)
                                        }),
                                    )
                                    .unwrap_or_else(|_| {
                                        eprintln!(
                                            "[steam_ipc] recovered from panic in IPC handler"
                                        );
                                        None
                                    });
                                    if let Some(resp) = response {
                                        if let Ok(js) = resp.to_json() {
                                            let _ = stream.write_all(js.as_bytes());
                                        }
                                    }
                                }
                            }
                            let _ = stream.shutdown(Shutdown::Both);
                        });
                    }
                    Err(_) => {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                }
            }
        });

        Ok(())
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if self.listener.take().is_some() {
            // Wake up blocking accept() cleanly and remove only a socket this server owned
            let _ = UnixStream::connect(&self.socket_path);
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Client side: runs in the GUI process, connects to the IPC socket.
#[derive(Debug, Clone)]
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
        Self::with_socket_path(ipc_socket_path())
    }

    pub fn with_socket_path(path: impl AsRef<Path>) -> Self {
        Self {
            socket_path: path.as_ref().to_path_buf(),
        }
    }

    pub fn send(&self, msg: &IpcMessage) -> Result<Option<IpcMessage>, String> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .map_err(|e| format!("Failed to connect to IPC socket: {}", e))?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

        let json = msg
            .to_json()
            .map_err(|e| format!("Serialization error: {}", e))?;
        stream
            .write_all(json.as_bytes())
            .map_err(|e| format!("Send error: {}", e))?;
        let _ = stream.shutdown(Shutdown::Write);

        let mut raw = String::new();
        stream
            .read_to_string(&mut raw)
            .map_err(|e| format!("Receive error: {}", e))?;

        if raw.trim().is_empty() {
            return Ok(None);
        }

        let reply = IpcMessage::from_json(raw.trim())
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
    fn live_unix_socket_server_and_client_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("test_ipc.sock");

        let mut srv = IpcServer::with_socket_path(sock.clone());
        srv.set_handler(|msg| match msg {
            IpcMessage::Ping => Some(IpcMessage::Pong {
                active: true,
                version: "2.5.1".into(),
            }),
            IpcMessage::SetManifestPin {
                app_id,
                depot_id,
                manifest_id,
                ..
            } => Some(IpcMessage::ManifestBound {
                app_id,
                depot_id,
                manifest_id,
                active: true,
            }),
            _ => None,
        });
        srv.start().unwrap();

        let client = IpcClient::with_socket_path(&sock);
        assert!(client.ping().unwrap());

        let reply = client
            .send(&IpcMessage::SetManifestPin {
                app_id: 1245620,
                depot_id: 1245621,
                manifest_id: "abc123".into(),
                size: 1024,
            })
            .unwrap()
            .unwrap();
        assert!(matches!(
            reply,
            IpcMessage::ManifestBound {
                app_id,
                depot_id,
                manifest_id,
                active
            } if app_id == 1245620 && depot_id == 1245621 && manifest_id == "abc123" && active
        ));

        srv.stop();
    }

    #[test]
    fn message_variants_serialize_cleanly() {
        let msgs = vec![
            IpcMessage::SetStat {
                app_id: 1245620,
                stat_name: "steamid".into(),
                stat_value: "76561198028121353".into(),
            },
            IpcMessage::RemoveStat {
                app_id: 1245620,
                stat_name: "steamid".into(),
            },
            IpcMessage::SetLegacyCdKey {
                app_id: 1245620,
                key: "ABCD-EFGH-IJKL".into(),
            },
            IpcMessage::RemoveLegacyCdKey { app_id: 1245620 },
            IpcMessage::SetDepotKey {
                depot_id: 1245621,
                key: "01".repeat(32),
            },
            IpcMessage::RemoveDepotKey { depot_id: 1245621 },
            IpcMessage::SetAppIdSpoof {
                app_id: 1245620,
                spoof_id: 480,
            },
            IpcMessage::RemoveAppIdSpoof { app_id: 1245620 },
        ];
        for m in msgs {
            let js = m.to_json().unwrap();
            assert!(IpcMessage::from_json(&js).is_ok());
        }
    }
}
