use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use command::blocking::Command;
use instant::Instant;

use super::peer::ChildGuard;

pub fn generate(path: &Path, size: u64) {
    let mut file = File::create(path).expect("create fixture");
    let mut chunk = [0; 64 * 1024];
    let mut offset = 0;
    while offset < size {
        let count = (size - offset).min(chunk.len() as u64) as usize;
        for (index, byte) in chunk[..count].iter_mut().enumerate() {
            let position = offset + index as u64;
            // Each aligned block includes all 256 values, but distant blocks differ.
            *byte = (position as u8).wrapping_add(((position >> 16) % 251) as u8);
        }
        file.write_all(&chunk[..count])
            .expect("write fixture chunk");
        offset += count as u64;
    }
    file.sync_all().expect("sync fixture");
}

pub fn small_batch(directory: &Path) -> Vec<PathBuf> {
    [
        ("all bytes.bin", 32 * 1024 + 17),
        ("empty.bin", 0),
        ("\u{4e2d}\u{6587} space.txt", 4097),
    ]
    .into_iter()
    .map(|(name, size)| {
        let path = directory.join(name);
        generate(&path, size);
        path
    })
    .collect()
}

fn sha256(path: &Path) -> String {
    let mut output = tempfile::tempfile().expect("hash output");
    let mut command = Command::new("shasum");
    command
        .args(["-a", "256"])
        .stdin(File::open(path).expect("open hash input"))
        .stdout(output.try_clone().expect("clone hash output"));
    let mut child = ChildGuard::spawn(&mut command);
    assert!(
        child
            .wait_until(Instant::now() + Duration::from_secs(120))
            .success()
    );
    output.seek(SeekFrom::Start(0)).expect("rewind hash output");
    let mut text = String::new();
    output
        .take(1024)
        .read_to_string(&mut text)
        .expect("read hash output");
    let digest = text.split_whitespace().next().expect("SHA-256 digest");
    assert_eq!(digest.len(), 64, "invalid SHA-256 output: {text:?}");
    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    digest.to_owned()
}

pub fn assert_equal(source: &Path, destination: &Path) {
    let mut left = File::open(source).expect("open source");
    let mut right = File::open(destination).expect("open received file");
    let size = left.metadata().expect("source metadata").len();
    assert_eq!(size, right.metadata().expect("destination metadata").len());
    let mut left_chunk = [0; 64 * 1024];
    let mut right_chunk = [0; 64 * 1024];
    let mut offset = 0;
    while offset < size {
        let count = (size - offset).min(left_chunk.len() as u64) as usize;
        left.read_exact(&mut left_chunk[..count])
            .expect("read source chunk");
        right
            .read_exact(&mut right_chunk[..count])
            .expect("read destination chunk");
        assert_eq!(
            &left_chunk[..count],
            &right_chunk[..count],
            "offset {offset}"
        );
        offset += count as u64;
    }
    let hash = sha256(source);
    assert_eq!(hash, sha256(destination), "SHA-256 mismatch");
    eprintln!(
        "verified {}: {size} bytes SHA-256 {hash}",
        destination.display()
    );
}

pub fn entries(directory: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .expect("read destination directory")
        .map(|entry| entry.expect("directory entry").path())
        .collect();
    paths.sort();
    paths
}

pub fn peak_rss_bytes() -> u64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the struct on success.
    let usage = unsafe {
        assert_eq!(libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()), 0);
        usage.assume_init()
    };
    #[cfg(target_os = "macos")]
    let scale = 1;
    #[cfg(not(target_os = "macos"))]
    let scale = 1024;
    usage.ru_maxrss as u64 * scale
}
