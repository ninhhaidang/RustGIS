//! Decoded-byte buffers: reserve only for bytes received, within the format's existing limit.
use std::io::{ErrorKind, Read};

use crate::{DataError, Result};

/// Reads a decoder to EOF without reserving its claimed output length. Capacity grows with the
/// bytes actually received, never past `limit`; an extra byte is an error, not truncation.
/// Cancellation is checked between reads of at most 64 KiB. `what` names the format and codec
/// in failures. The limit bounds this buffer's capacity, not the decoder's own scratch memory.
pub(super) fn read(mut decoder: impl Read, limit: usize, what: &str) -> Result<Vec<u8>> {
    let failure = |message: &str| DataError::Format(format!("{what}: {message}"));
    let mut out = Vec::new();
    let mut buffer = [0u8; 64 << 10];
    loop {
        crate::progress::check()?;
        let remaining = limit.saturating_sub(out.len());
        // Probe one byte beyond the limit, including when the allowed output is empty.
        let take = remaining.saturating_add(1).min(buffer.len());
        let target = buffer.get_mut(..take).ok_or_else(|| failure("invalid read size"))?;
        let n = match decoder.read(target) {
            Ok(0) => return Ok(out),
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                let message = e.to_string();
                return Err(match DataError::from(e) {
                    DataError::Cancelled => DataError::Cancelled,
                    _ => failure(&message),
                });
            }
        };
        let bytes = buffer.get(..n).ok_or_else(|| failure("decoder returned an invalid byte count"))?;
        if n > remaining {
            return Err(failure("decoded data is too large"));
        }
        if out.capacity().saturating_sub(out.len()) < n {
            let next = out.len() + n; // n <= limit - out.len(), checked above.
            let capacity = out.capacity().saturating_mul(2).max(next).min(limit);
            out.try_reserve_exact(capacity - out.len()).map_err(|_| failure("not enough memory for the decoded data"))?;
        }
        out.extend_from_slice(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::Ordering};

    #[test]
    fn limits_accept_complete_data_and_refuse_an_extra_byte() {
        for limit in [0, 3, 4, 8, usize::MAX] {
            let result = read(b"abcd".as_slice(), limit, "test decoder");
            if limit < 4 {
                assert!(matches!(result, Err(DataError::Format(ref message)) if message.contains("too large")));
            } else {
                let bytes = result.unwrap();
                assert_eq!(bytes, b"abcd");
                assert!(bytes.capacity() <= limit);
            }
        }
        assert!(read(b"".as_slice(), 0, "empty decoder").unwrap().is_empty());
    }

    struct InterruptedOnce(bool);
    impl Read for InterruptedOnce {
        /// A transient interruption must not be mistaken for EOF or a permanent decoder error.
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if !self.0 {
                self.0 = true;
                return Err(std::io::Error::from(ErrorKind::Interrupted));
            }
            b"ok".as_slice().read(bytes)
        }
    }

    struct Failed;
    impl Read for Failed {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("decoder failure"))
        }
    }

    #[test]
    fn interruptions_retry_but_read_failures_are_not_partial_success() {
        // An endless stream after its first interruption: the limit still stops it.
        assert!(matches!(read(InterruptedOnce(false), 3, "interrupted decoder"), Err(DataError::Format(ref s)) if s.contains("too large")));
        assert!(matches!(read(Failed, 10, "failed decoder"), Err(DataError::Format(ref s)) if s.contains("decoder failure")));
    }

    struct Stops {
        probe: Arc<crate::progress::tests::Counted>,
        calls: usize,
    }
    impl Read for Stops {
        /// Cancellation after the first read must stop before asking the decoder for more.
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            *bytes.first_mut().ok_or_else(|| std::io::Error::other("empty read"))? = 1;
            self.calls += 1;
            self.probe.cancel.store(true, Ordering::Relaxed);
            Ok(1)
        }
    }

    #[test]
    fn cancellation_between_reads_keeps_its_error_kind() {
        let probe = Arc::new(crate::progress::tests::Counted::default());
        let _guard = crate::progress::install(Some(probe.clone()));
        let mut decoder = Stops { probe, calls: 0 };
        assert!(matches!(read(&mut decoder, 10, "cancelled decoder"), Err(DataError::Cancelled)));
        assert_eq!(decoder.calls, 1);
    }
}
