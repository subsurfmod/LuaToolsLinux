//! Linux implementation of OSTPlatform - OS abstraction layer for Steam integration.
//!
//! This module provides Linux equivalents of the Windows OSTPlatform functions,
//! enabling the usefulsteamtools codebase to run on Linux via Rust.

pub mod dynamic_library;
pub mod process;
pub mod remote_process;
pub mod memory;
pub mod byte_search;
pub mod directory_watch;
pub mod steam_credential_store;
pub mod bkv_parser;

// Re-export all dynamic_library symbols with full API compatibility
pub use dynamic_library::{
    load, get_loaded, get_symbol, get_symbol_ordinal, get_last_error_code,
    get_current_directory_path, get_system_directory_path, get_module_directory,
    get_main_executable_path, DynamicLibraryError,
    is_valid_module, unload, list_loaded_modules, dll_to_so_name,
};

// Re-export ModuleHandle type alias for backwards compatibility
pub type ModuleHandle = dynamic_library::ModuleHandle;

pub use process::{
    get_creation_time, format_creation_time, get_image_path, get_environment_variable_value,
    get_process_command_line, enumerate_modules, is_system_module_path, launch_detached_hidden,
    ModuleInfo,
};

pub use remote_process::{get_architecture, inject_library, Architecture, InjectStatus};

pub use memory::{get_module_image, write_executable_byte, is_readable, ModuleImage};
pub use byte_search::{find, find_with_mask, find_in_file, find_in_file_range};
pub use directory_watch::{Watch, wait_any, Change, ChangeAction, WaitResult, WaitStatus};
pub use steam_credential_store::{get_app_ticket, write_app_ticket, get_e_ticket, write_e_ticket,
                                 get_steam_id, write_steam_id, get_active_user, initialize,
                                 Status, to_string};
pub use bkv_parser::{BkvValue, BkvError, BkvReader, parse_bkv_file, extract_achievements,
                     AchievementData, find_user_game_stats_file};