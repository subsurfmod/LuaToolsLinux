//! Linux implementation of OSTPlatform - OS abstraction layer for Steam integration.
//!
//! This module provides Linux equivalents of the Windows OSTPlatform functions,
//! enabling the usefulsteamtools codebase to run on Linux via Rust.

pub mod bkv_parser;
pub mod byte_search;
pub mod keyvalues1;
pub mod dialog;
pub mod directory_watch;
pub mod dynamic_library;
pub mod encoding;
pub mod hash;
pub mod http;
pub mod memory;
pub mod numbers;
pub mod pe;
pub mod process;
pub mod remote_process;
pub mod steam_credential_store;
pub mod thread;
pub mod trap;

// Re-export all dynamic_library symbols with full API compatibility
pub use dynamic_library::{
    dll_to_so_name, get_current_directory_path, get_last_error_code, get_loaded,
    get_main_executable_path, get_module_directory, get_symbol, get_symbol_ordinal,
    get_system_directory_path, is_valid_module, list_loaded_modules, load, unload,
    DynamicLibraryError,
};

// Re-export ModuleHandle type alias for backwards compatibility
pub type ModuleHandle = dynamic_library::ModuleHandle;

pub use process::{
    enumerate_modules, format_creation_time, get_creation_time, get_environment_variable_value,
    get_image_path, get_process_command_line, is_system_module_path, launch_detached_hidden,
    ModuleInfo,
};

pub use remote_process::{get_architecture, inject_library, Architecture, InjectStatus};

pub use bkv_parser::{
    extract_achievements, find_user_game_stats_file, parse_bkv_file, AchievementData, BkvError,
    BkvReader, BkvValue,
};

// New Valve KV1 parser (replaces custom BKV format for UserGameStats_*.bin)
pub use keyvalues1::{
    extract_achievements_kv1, extract_achievements_with_schema,
    find_user_game_stats_file as find_user_game_stats_file_kv1, parse_kv1_bytes, parse_kv1_file,
    write_kv1, AchievementData as Kv1AchievementData, Kv1Error, Kv1Reader, Kv1Value,
};
pub use byte_search::{find, find_in_file, find_in_file_range, find_with_mask};
pub use directory_watch::{wait_any, Change, ChangeAction, WaitResult, WaitStatus, Watch};
pub use memory::{get_module_image, is_readable, write_executable_byte, ModuleImage};
pub use steam_credential_store::{
    get_active_user, get_app_ticket, get_e_ticket, get_steam_id, initialize, to_string,
    write_app_ticket, write_e_ticket, write_steam_id, Status,
};
