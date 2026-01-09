mod cache;
mod config;
pub(crate) mod jsonc;
mod paths;
mod profile;
pub mod utils;

pub use cache::{ensure_cache_dirs, rules_profile_dir, settings_profile_dir, CacheDirs};
pub use config::{AgentConfig, CatalogEntry, ConfigFile, McpServer, Profile};
pub use paths::{
    backups_dir, base_config_dir, commands_dir, config_file_path, rules_dir, settings_dir,
    skills_dir,
};
pub use profile::{
    create_profile, get_profile, list_profiles, remove_profile, rename_profile, switch_profile,
    update_profile, AgentScope, ProfileDraft, ProfilePatch, SwitchReport,
};
