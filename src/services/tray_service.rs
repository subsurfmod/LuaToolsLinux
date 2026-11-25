//! TrayService — System tray icon support with minimize-to-tray functionality.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    TrayIcon, TrayIconBuilder,
};
use winit::event_loop::EventLoopProxy;

use crate::ui::state::AppMessage;

#[derive(Clone)]
pub enum TrayEvent {
    ShowWindow,
    HideWindow,
    ToggleWindow,
    Quit,
    RestartSteam,
    OpenSettings,
}

pub struct TrayService {
    tray_icon: Option<TrayIcon>,
    event_loop_proxy: Option<EventLoopProxy<TrayEvent>>,
    app_message_tx: Arc<Mutex<Option<std::sync::mpsc::Sender<AppMessage>>>>,
    is_visible: Arc<Mutex<bool>>,
}

impl TrayService {
    pub fn new() -> Self {
        Self {
            tray_icon: None,
            event_loop_proxy: None,
            app_message_tx: Arc::new(Mutex::new(None)),
            is_visible: Arc::new(Mutex::new(true)),
        }
    }

    pub fn set_app_message_sender(&mut self, tx: std::sync::mpsc::Sender<AppMessage>) {
        *self.app_message_tx.lock().unwrap() = Some(tx);
    }

    pub fn set_event_loop_proxy(&mut self, proxy: EventLoopProxy<TrayEvent>) {
        self.event_loop_proxy = Some(proxy);
    }

    pub fn set_visibility(&self, visible: bool) {
        *self.is_visible.lock().unwrap() = visible;
    }

    pub fn is_visible(&self) -> bool {
        *self.is_visible.lock().unwrap()
    }

    pub fn initialize(&mut self) -> Result<(), String> {
        // Build tray menu
        let show_item = MenuItem::new("Show LuaTools", true, None);
        let hide_item = MenuItem::new("Hide to Tray", true, None);
        let separator = PredefinedMenuItem::separator();
        let restart_steam_item = MenuItem::new("Restart Steam with Bridge", true, None);
        let settings_item = MenuItem::new("Settings", true, None);
        let separator2 = PredefinedMenuItem::separator();
        let quit_item = MenuItem::new("Quit", true, None);

        let menu = Menu::new();
        menu.append(&show_item).map_err(|e| e.to_string())?;
        menu.append(&hide_item).map_err(|e| e.to_string())?;
        menu.append(&separator).map_err(|e| e.to_string())?;
        menu.append(&restart_steam_item).map_err(|e| e.to_string())?;
        menu.append(&settings_item).map_err(|e| e.to_string())?;
        menu.append(&separator2).map_err(|e| e.to_string())?;
        menu.append(&quit_item).map_err(|e| e.to_string())?;

        // Create tray icon with default icon
        let icon = Self::create_tray_icon();

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("LuaTools Linux")
            .with_icon(icon)
            .build()
            .map_err(|e| format!("Failed to create tray icon: {}", e))?;

        self.tray_icon = Some(tray);

        // Start menu event handler
        self.start_menu_handler();

        Ok(())
    }

    fn create_tray_icon() -> tray_icon::Icon {
        // Create a simple 32x32 icon (blue square with white L)
        let mut rgba = vec![0u8; 32 * 32 * 4];
        for y in 0..32 {
            for x in 0..32 {
                let idx = (y * 32 + x) * 4;
                let color = if (x >= 4 && x <= 27 && y >= 4 && y <= 27) {
                    // Blue background
                    [26, 58, 107, 255]
                } else {
                    // Transparent
                    [0, 0, 0, 0]
                };
                rgba[idx..idx + 4].copy_from_slice(&color);
            }
        }
        // Draw a white "L" in the center
        for y in 10..22 {
            let idx = (y * 32 + 10) * 4;
            rgba[idx..idx + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
        for x in 10..22 {
            let idx = (21 * 32 + x) * 4;
            rgba[idx..idx + 4].copy_from_slice(&[255, 255, 255, 255]);
        }

        tray_icon::Icon::from_rgba(rgba, 32, 32).unwrap()
    }

    fn start_menu_handler(&self) {
        let tx = self.app_message_tx.clone();
        let visibility = self.is_visible.clone();
        let proxy = self.event_loop_proxy.clone();

        thread::spawn(move || {
            let receiver = MenuEvent::receiver();
            for event in receiver.iter() {
                match event.id().0.as_str() {
                    "Show LuaTools" => {
                        *visibility.lock().unwrap() = true;
                        if let Some(tx) = tx.lock().unwrap().as_ref() {
                            let _ = tx.send(AppMessage::ShowWindow);
                        }
                    }
                    "Hide to Tray" => {
                        *visibility.lock().unwrap() = false;
                        if let Some(tx) = tx.lock().unwrap().as_ref() {
                            let _ = tx.send(AppMessage::HideWindow);
                        }
                    }
                    "Restart Steam with Bridge" => {
                        if let Some(tx) = tx.lock().unwrap().as_ref() {
                            let _ = tx.send(AppMessage::RestartSteamRequested);
                        }
                    }
                    "Settings" => {
                        *visibility.lock().unwrap() = true;
                        if let Some(tx) = tx.lock().unwrap().as_ref() {
                            let _ = tx.send(AppMessage::OpenSettings);
                        }
                    }
                    "Quit" => {
                        if let Some(tx) = tx.lock().unwrap().as_ref() {
                            let _ = tx.send(AppMessage::QuitRequested);
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    pub fn update_tooltip(&self, text: &str) {
        if let Some(tray) = &self.tray_icon {
            tray.set_tooltip(Some(text)).ok();
        }
    }

    pub fn show_notification(&self, title: &str, body: &str) {
        if let Some(tray) = &self.tray_icon {
            tray.show_notification(
                tray_icon::Notification::new()
                    .with_title(title)
                    .with_body(body)
                    .with_notification_type(tray_icon::NotificationType::Info),
            ).ok();
        }
    }

    pub fn cleanup(&mut self) {
        self.tray_icon = None;
    }
}

impl Default for TrayService {
    fn default() -> Self {
        Self::new()
    }
}