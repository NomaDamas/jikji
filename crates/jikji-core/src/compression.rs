use std::io::{Error, ErrorKind, Read};

use crate::{Result, io_error};

pub const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

pub fn compress_body_text(text: &str) -> Result<Vec<u8>> {
    let length = text.len();
    if length > MAX_BODY_BYTES {
        return Err(io_error(
            "artifact body",
            Error::new(
                ErrorKind::InvalidInput,
                "artifact body exceeds compression cap",
            ),
        ));
    }
    zstd::bulk::compress(text.as_bytes(), 3).map_err(|source| io_error("artifact body", source))
}

pub fn decompress_body_text(compressed: &[u8], original_size: usize) -> Result<String> {
    if original_size > MAX_BODY_BYTES {
        return Err(io_error(
            "artifact body",
            Error::new(
                ErrorKind::InvalidData,
                "artifact body exceeds decompression cap",
            ),
        ));
    }
    let mut decoder = zstd::stream::read::Decoder::new(compressed)
        .map_err(|source| io_error("artifact body", source))?;
    let mut bytes = Vec::with_capacity(original_size);
    decoder
        .by_ref()
        .take((original_size as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| io_error("artifact body", source))?;
    if bytes.len() != original_size {
        return Err(io_error(
            "artifact body",
            Error::new(ErrorKind::InvalidData, "artifact body length mismatch"),
        ));
    }
    String::from_utf8(bytes)
        .map_err(|source| io_error("artifact body", Error::new(ErrorKind::InvalidData, source)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_and_empty_round_trip() {
        for input in ["", "안녕하세요 🌍\n\0"] {
            let compressed = compress_body_text(input).unwrap();
            assert_eq!(
                decompress_body_text(&compressed, input.len()).unwrap(),
                input
            );
        }
    }

    #[test]
    fn rejects_corrupt_and_wrong_length() {
        let compressed = compress_body_text("hello").unwrap();
        assert!(decompress_body_text(&compressed, 4).is_err());
        assert!(decompress_body_text(b"not zstd", 8).is_err());
    }
}
