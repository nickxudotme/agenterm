use super::*;

#[test]
fn progress_creates_pending_file() {
    let mut transfer = ZmodemTransfer::new();
    transfer.note_progress("a.txt".to_owned(), 0, 100);
    assert_eq!(transfer.pending.as_ref().unwrap().name, "a.txt");
    assert_eq!(transfer.pending.as_ref().unwrap().bytes_total, 100);
}

#[test]
fn appends_data_to_pending_file() {
    let mut transfer = ZmodemTransfer::new();
    transfer.note_progress("a.txt".to_owned(), 0, 4);
    transfer.append_data(b"ab");
    transfer.append_data(b"cd");
    assert_eq!(transfer.pending.as_ref().unwrap().bytes, b"abcd");
}

#[test]
fn append_without_pending_file_is_ignored() {
    let mut transfer = ZmodemTransfer::new();
    transfer.append_data(b"orphan");
    assert!(transfer.pending.is_none());
}

#[test]
fn new_file_name_replaces_stale_buffer() {
    let mut transfer = ZmodemTransfer::new();
    transfer.note_progress("a.txt".to_owned(), 0, 2);
    transfer.append_data(b"ab");
    // No completion arrived before the next file started.
    transfer.note_progress("b.txt".to_owned(), 0, 3);
    let pending = transfer.pending.as_ref().unwrap();
    assert_eq!(pending.name, "b.txt");
    assert!(pending.bytes.is_empty());
}

#[test]
fn repeated_progress_for_same_file_keeps_buffer() {
    let mut transfer = ZmodemTransfer::new();
    transfer.note_progress("a.txt".to_owned(), 0, 10);
    transfer.append_data(b"xy");
    transfer.note_progress("a.txt".to_owned(), 2, 10);
    assert_eq!(transfer.pending.as_ref().unwrap().bytes, b"xy");
}

#[test]
fn writes_file_to_disk() {
    let dir = std::env::temp_dir().join(format!("zmodem-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("out.bin");

    let written = write_file(&path, b"payload").expect("write succeeds");
    assert_eq!(written, 7);
    assert_eq!(std::fs::read(&path).unwrap(), b"payload");

    std::fs::remove_file(&path).ok();
    std::fs::remove_dir(&dir).ok();
}

#[test]
fn write_file_reports_missing_directory() {
    let path = PathBuf::from("/definitely/not/a/real/dir/out.bin");
    assert!(write_file(&path, b"x").is_err());
}
