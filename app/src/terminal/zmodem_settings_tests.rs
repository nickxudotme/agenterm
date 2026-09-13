use settings::Setting;

use super::*;

fn default_settings() -> ZmodemSettings {
    ZmodemSettings {
        enabled: ZmodemEnabled::new(None),
        ask_download_directory: ZmodemAskDownloadDirectory::new(None),
        download_directory: ZmodemDownloadDirectory::new(None),
        last_directory: ZmodemLastDirectory::new(None),
        overwrite_policy: ZmodemOverwritePolicySetting::new(None),
        drag_enabled: ZmodemDragEnabled::new(None),
        upload_command: ZmodemUploadCommand::new(None),
        cross_transfer_enabled: ZmodemCrossTransferEnabled::new(None),
    }
}

#[test]
fn defaults_require_directory_choice_and_preserve_existing_files() {
    let settings = default_settings();
    assert!(*settings.enabled.value());
    assert!(*settings.ask_download_directory.value());
    assert_eq!(
        *settings.overwrite_policy.value(),
        ZmodemOverwritePolicy::Skip
    );
    assert!(!*settings.drag_enabled.value());
    assert!(!*settings.cross_transfer_enabled.value());
    assert_eq!(settings.upload_command.value(), "rz");
    assert!(settings.last_directory.is_empty());
    assert_eq!(
        settings.resolved_download_directory(),
        default_download_directory()
    );
}

fn assert_metadata<S: Setting>(toml_path: Option<&str>) {
    assert!(matches!(S::sync_to_cloud(), SyncToCloud::Never));
    assert!(matches!(S::supported_platforms(), SupportedPlatforms::MAC));
    assert_eq!(S::toml_path(), toml_path);
    assert_eq!(S::is_private(), toml_path.is_none());
}

#[test]
fn all_settings_are_local_and_last_directory_is_private() {
    assert_metadata::<ZmodemEnabled>(Some("terminal.zmodem.enabled"));
    assert_metadata::<ZmodemAskDownloadDirectory>(Some("terminal.zmodem.ask_download_directory"));
    assert_metadata::<ZmodemDownloadDirectory>(Some("terminal.zmodem.download_directory"));
    assert_metadata::<ZmodemLastDirectory>(None);
    assert_metadata::<ZmodemOverwritePolicySetting>(Some("terminal.zmodem.overwrite_policy"));
    assert_metadata::<ZmodemDragEnabled>(Some("terminal.zmodem.drag_enabled"));
    assert_metadata::<ZmodemUploadCommand>(Some("terminal.zmodem.upload_command"));
    assert_metadata::<ZmodemCrossTransferEnabled>(Some("terminal.zmodem.cross_transfer_enabled"));
}

#[test]
fn overwrite_policy_roundtrips_as_snake_case() {
    for (policy, serialized) in [
        (ZmodemOverwritePolicy::Skip, "\"skip\""),
        (ZmodemOverwritePolicy::Rename, "\"rename\""),
        (ZmodemOverwritePolicy::Overwrite, "\"overwrite\""),
    ] {
        assert_eq!(serde_json::to_string(&policy).unwrap(), serialized);
        assert_eq!(
            serde_json::from_str::<ZmodemOverwritePolicy>(serialized).unwrap(),
            policy,
        );
    }
    assert!(serde_json::from_str::<ZmodemOverwritePolicy>("\"unknown\"").is_err());
}

#[test]
fn blank_directory_resolves_downloads_without_rewriting_explicit_paths() {
    let mut settings = default_settings();
    for blank in ["", "   "] {
        settings.download_directory = ZmodemDownloadDirectory::new(Some(blank.to_owned()));
        assert_eq!(
            settings.resolved_download_directory(),
            default_download_directory()
        );
    }
    let explicit_path = "/tmp/download folder ";
    settings.download_directory = ZmodemDownloadDirectory::new(Some(explicit_path.to_owned()));
    assert_eq!(
        settings.resolved_download_directory(),
        Some(PathBuf::from(explicit_path)),
    );
}
