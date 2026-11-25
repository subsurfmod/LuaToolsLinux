//! LuaTools Linux — Core library modules.

pub mod models;
pub mod parsers;
pub mod services;
pub mod steam_bridge;
pub mod steamflatline;
pub mod ui;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BUILD_GIT_SHA: &str = env!("LUATOOLS_BUILD_GIT_SHA");
