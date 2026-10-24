use anyhow::{Result, anyhow};
use std::io::{IsTerminal, Read};

/// Largest piped input sent to the model; longer input is cut with a notice.
pub const MAX_STDIN_BYTES: usize = 256 * 1024;

/// Piped input after the size limit.
#[derive(Debug, PartialEq, Eq)]
pub struct LimitedInput {
    pub text: String,
    pub truncated: bool,
}

/// Read stdin when it is a pipe or a regular file (`cmd | qq`, `qq < file`).
///
/// A terminal, `/dev/null`, a socket or other device is not read, so qq does
/// not block when a caller (an editor, an agent, CI) leaves stdin open but
/// never writes to it. Input longer than [`MAX_STDIN_BYTES`] is cut, with a
/// notice on stderr; binary input is an error.
pub fn read_piped_stdin() -> Result<Option<String>> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() || !stdin_is_pipe_or_file() {
        return Ok(None);
    }
    let input = read_limited(stdin.lock(), MAX_STDIN_BYTES)?;
    if input.truncated {
        eprintln!(
            "Note: piped input is longer than {} KiB; only the first {} KiB is sent.",
            MAX_STDIN_BYTES / 1024,
            MAX_STDIN_BYTES / 1024
        );
    }
    Ok(Some(input.text))
}

/// Read at most `max` bytes. Invalid UTF-8 is replaced; NUL bytes mean binary data.
pub fn read_limited<R: Read>(reader: R, max: usize) -> Result<LimitedInput> {
    let mut bytes = Vec::new();
    reader.take(max as u64 + 1).read_to_end(&mut bytes)?;
    let truncated = bytes.len() > max;
    bytes.truncate(max);
    if bytes.contains(&0) {
        return Err(anyhow!(
            "Piped input looks like binary data. Pipe text, or pass --no-stdin to ignore stdin."
        ));
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        // A cut inside a multi-byte character leaves one replacement character.
        if text.ends_with('\u{FFFD}') {
            text.pop();
        }
    }
    Ok(LimitedInput { text, truncated })
}

#[cfg(unix)]
fn stdin_is_pipe_or_file() -> bool {
    use std::os::fd::AsFd;
    use std::os::unix::fs::FileTypeExt;

    let Ok(fd) = std::io::stdin().as_fd().try_clone_to_owned() else {
        return false;
    };
    match std::fs::File::from(fd).metadata() {
        Ok(meta) => {
            let kind = meta.file_type();
            kind.is_fifo() || kind.is_file()
        }
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn stdin_is_pipe_or_file() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_input_is_kept_whole() {
        let got = read_limited("hello\n".as_bytes(), 10).unwrap();
        assert_eq!(
            got,
            LimitedInput {
                text: "hello\n".into(),
                truncated: false
            }
        );
    }

    #[test]
    fn input_at_the_limit_is_not_truncated() {
        let got = read_limited("abcd".as_bytes(), 4).unwrap();
        assert!(!got.truncated);
        assert_eq!(got.text, "abcd");
    }

    #[test]
    fn long_input_is_cut_at_the_limit() {
        let got = read_limited("abcdef".as_bytes(), 4).unwrap();
        assert!(got.truncated);
        assert_eq!(got.text, "abcd");
    }

    #[test]
    fn cut_inside_a_character_drops_the_partial_character() {
        let got = read_limited("aż".as_bytes(), 2).unwrap();
        assert!(got.truncated);
        assert_eq!(got.text, "a");
    }

    #[test]
    fn invalid_utf8_is_replaced() {
        let got = read_limited(&b"caf\x83e"[..], 100).unwrap();
        assert_eq!(got.text, "caf\u{FFFD}e");
    }

    #[test]
    fn binary_input_is_an_error() {
        assert!(read_limited(&b"\x7fELF\x00\x01"[..], 100).is_err());
    }
}
