use std::path::PathBuf;

use strum_macros::EnumIter;
use warp_core::settings::macros::define_settings_group;
use warp_core::settings::{SupportedPlatforms, SyncToCloud};

#[derive(
    Default,
    Debug,
    serde::Serialize,
    serde::Deserialize,
    PartialEq,
    Eq,
    Copy,
    Clone,
    EnumIter,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum ZmodemOverwritePolicy {
    #[default]
    Skip,
    Rename,
    Overwrite,
}

impl ZmodemOverwritePolicy {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Skip => "Skip",
            Self::Rename => "Rename",
            Self::Overwrite => "Overwrite",
        }
    }
}

define_settings_group!(ZmodemSettings, settings: [
    enabled: ZmodemEnabled {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::MAC,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "terminal.zmodem.enabled",
        description: "Whether to enable ZMODEM file transfers in local PTY sessions.",
    },
    ask_download_directory: ZmodemAskDownloadDirectory {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::MAC,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "terminal.zmodem.ask_download_directory",
        description: "Whether to choose a download directory before each ZMODEM batch.",
    },
    download_directory: ZmodemDownloadDirectory {
        type: String,
        default: default_download_directory()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
        supported_platforms: SupportedPlatforms::MAC,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "terminal.zmodem.download_directory",
        description: "ZMODEM download directory. Blank uses the system Downloads directory.",
    },
    last_directory: ZmodemLastDirectory {
        type: String,
        default: String::new(),
        supported_platforms: SupportedPlatforms::MAC,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: true,
    },
    overwrite_policy: ZmodemOverwritePolicySetting {
        type: ZmodemOverwritePolicy,
        default: ZmodemOverwritePolicy::Skip,
        supported_platforms: SupportedPlatforms::MAC,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "terminal.zmodem.overwrite_policy",
        description: "How ZMODEM downloads handle existing files.",
    },
    drag_enabled: ZmodemDragEnabled {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::MAC,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "terminal.zmodem.drag_enabled",
        description: "Whether dropping files explicitly starts a ZMODEM upload.",
    },
    upload_command: ZmodemUploadCommand {
        type: String,
        default: "rz".to_owned(),
        supported_platforms: SupportedPlatforms::MAC,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "terminal.zmodem.upload_command",
        description: "Remote command used for an explicitly requested ZMODEM upload.",
    },
    cross_transfer_enabled: ZmodemCrossTransferEnabled {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::MAC,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "terminal.zmodem.cross_transfer_enabled",
        description: "Whether to allow explicit ZMODEM transfers between terminals.",
    },
]);

/// Returns None when the local user's home directory cannot be determined.
pub fn default_download_directory() -> Option<PathBuf> {
    directories::UserDirs::new().map(|dirs| {
        dirs.download_dir()
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs.home_dir().join("Downloads"))
    })
}

impl ZmodemSettings {
    pub fn resolved_download_directory(&self) -> Option<PathBuf> {
        if self.download_directory.trim().is_empty() {
            default_download_directory()
        } else {
            Some(PathBuf::from(self.download_directory.as_str()))
        }
    }
}

#[cfg(test)]
#[path = "zmodem_settings_tests.rs"]
mod tests;
