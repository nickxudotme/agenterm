use std::path::PathBuf;

use super::*;

#[test]
fn repeated_request_opens_only_one_picker() {
    let mut transfer = ZmodemTransfer::default();
    assert!(transfer.request(1, Role::Upload));
    assert!(!transfer.request(1, Role::Upload));
    assert!(transfer.is_selecting(1, Role::Upload, false));
    assert!(!transfer.is_selecting(1, Role::Upload, true));
}

#[test]
fn stale_picker_cannot_start_another_transfer() {
    let mut transfer = ZmodemTransfer::default();
    assert!(transfer.request(1, Role::Upload));
    transfer.finish(1, TransferOutcome::Cancelled);
    assert!(transfer.request(2, Role::Download));
    assert!(!transfer.is_selecting(1, Role::Upload, false));
    assert!(!transfer.request(1, Role::Upload));
    assert!(transfer.is_selecting(2, Role::Download, false));
}

#[test]
fn remote_request_invalidates_unstarted_explicit_picker() {
    let mut transfer = ZmodemTransfer::default();
    assert!(transfer.begin_explicit_upload(1));
    assert!(transfer.request(2, Role::Upload));
    assert!(!transfer.is_selecting(1, Role::Upload, true));
    assert!(transfer.is_selecting(2, Role::Upload, false));
}

#[test]
fn cancel_invalidates_picker_before_runtime_finishes() {
    let mut transfer = ZmodemTransfer::default();
    transfer.request(1, Role::Download);
    assert!(transfer.cancel(1));
    assert!(!transfer.is_selecting(1, Role::Download, false));
    assert_eq!(
        transfer.active.as_ref().unwrap().phase,
        TransferPhase::Cancelling
    );
}

#[test]
fn cancelled_explicit_picker_does_not_wait_for_nonexistent_worker() {
    let mut transfer = ZmodemTransfer::default();
    transfer.begin_explicit_upload(1);
    transfer.cancel(1);
    assert!(transfer.active.is_none());
    assert_eq!(
        transfer.last.as_ref().unwrap().outcome,
        TransferOutcome::Cancelled
    );
}

#[test]
fn simultaneous_terminals_keep_metadata_separate() {
    let mut first = ZmodemTransfer::default();
    let mut second = ZmodemTransfer::default();
    first.request(1, Role::Download);
    second.request(2, Role::Upload);
    let progress = TransferEvent::Progress {
        id: 1,
        role: Role::Download,
        name: "same.bin".to_owned(),
        bytes: 7,
        total: Some(10),
    };
    assert!(first.apply(&progress));
    assert!(!second.apply(&progress));
    assert_eq!(
        first
            .active
            .as_ref()
            .unwrap()
            .current_file
            .as_ref()
            .unwrap()
            .bytes,
        7
    );
    assert!(second.active.as_ref().unwrap().current_file.is_none());
}

#[test]
fn file_and_batch_results_retain_outcomes_and_skips() {
    let mut transfer = ZmodemTransfer::default();
    transfer.request(1, Role::Download);
    let path = PathBuf::from("/chosen/file.bin");
    for (name, outcome, path) in [
        ("file.bin", FileOutcome::Completed, Some(path.clone())),
        ("existing.bin", FileOutcome::Skipped, None),
    ] {
        transfer.apply(&TransferEvent::FileResult {
            id: 1,
            role: Role::Download,
            name: name.to_owned(),
            bytes: 4,
            outcome,
            path,
        });
    }
    assert!(transfer.apply(&TransferEvent::Finished {
        id: 1,
        role: Role::Download,
        outcome: TransferOutcome::Completed,
        committed_paths: vec![path.clone()],
    }));
    let last = transfer.last.as_ref().unwrap();
    assert_eq!(last.id, 1);
    assert!(
        transfer
            .status_text()
            .unwrap()
            .contains("1 completed, 1 skipped")
    );
    assert!(!transfer.request(1, Role::Download));
}

#[test]
fn upload_progress_and_control_characters_are_displayed_safely() {
    let mut transfer = ZmodemTransfer::default();
    transfer.begin_explicit_upload(1);
    transfer.start(1);
    transfer.apply(&TransferEvent::FileStarted {
        id: 1,
        role: Role::Upload,
        name: "a\x1b\n.txt".to_owned(),
        size: Some(0),
    });
    let status = transfer.status_text().unwrap();
    assert!(status.starts_with("Upload:"));
    assert!(status.contains("0 / 0 bytes"));
    assert!(!status.chars().any(char::is_control));
}
