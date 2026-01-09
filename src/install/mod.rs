mod discover;
mod extract;
mod installer;
mod source;
mod uninstaller;

pub use discover::{DiscoveredCommand, DiscoveredSkill, Discovery};
pub use installer::{
    discover_from_source, install_from_discovery, install_from_source, InstallReport,
};
pub use uninstaller::{
    uninstall_from_profiles, uninstall_from_profiles_in_config, UninstallReport,
};
