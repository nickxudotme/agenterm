use super::*;

#[test]
fn parse_empty_file_size_as_zero() {
    assert_eq!(parse_file_size(b""), Ok(0));
}

#[test]
fn reject_bad_file_size() {
    assert_eq!(parse_file_size(b"12x"), Err(Error::MalformedFileSize));
}
