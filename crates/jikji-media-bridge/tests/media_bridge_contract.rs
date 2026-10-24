use std::fs;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::time::{Duration, Instant};

use jikji_media_bridge::{
    BridgeAvailability, BridgeRuntime, MediaBridgeRequest, MediaBridgeStatus, MediaKind,
};
#[cfg(unix)]
use jikji_media_bridge::{BridgeConfig, NativeEngineConfig};
use tempfile::tempdir;

#[test]
fn image_metadata_is_extracted_without_text_engine() {
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("sample.png");
    write_png(&path, 31, 47);

    let runtime = BridgeRuntime::new();
    let outcome = runtime.extract(&MediaBridgeRequest::new(path, MediaKind::Image));

    assert_eq!(runtime.availability(), BridgeAvailability::Native);
    assert_eq!(outcome.status, MediaBridgeStatus::MetadataOnly);
    assert_eq!(
        outcome.metadata.get("engine").map(String::as_str),
        Some("rust-native")
    );
    assert_eq!(
        outcome.metadata.get("width").map(String::as_str),
        Some("31")
    );
    assert_eq!(
        outcome.metadata.get("height").map(String::as_str),
        Some("47")
    );
}

#[test]
fn wav_metadata_is_extracted_without_text_engine() {
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("sample.wav");
    write_wav(&path, 8_000, 1, 16, 16_000);

    let outcome = BridgeRuntime::new().extract(&MediaBridgeRequest::new(path, MediaKind::Audio));

    assert_eq!(outcome.status, MediaBridgeStatus::MetadataOnly);
    assert_eq!(outcome.text, "");
    assert_eq!(
        outcome.metadata.get("channels").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        outcome.metadata.get("sample_rate_hz").map(String::as_str),
        Some("8000")
    );
    assert_eq!(
        outcome.metadata.get("bits_per_sample").map(String::as_str),
        Some("16")
    );
    assert_eq!(
        outcome.metadata.get("duration_ms").map(String::as_str),
        Some("1000")
    );
}

#[test]
fn missing_media_is_unavailable_without_panic() {
    let outcome = BridgeRuntime::new().extract(&MediaBridgeRequest::new(
        Path::new("/missing/jikji-media.mp4").to_path_buf(),
        MediaKind::Video,
    ));

    assert_eq!(outcome.status, MediaBridgeStatus::Unavailable);
    // The OS wording differs: "No such file" (Unix), "cannot find the file specified" (Windows).
    assert!(
        !outcome.error.is_empty(),
        "missing media must explain itself"
    );
}

#[test]
fn malformed_media_remains_metadata_only() {
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("broken.jpg");
    fs::write(&path, b"not-a-jpeg").expect("write fixture");

    let outcome = BridgeRuntime::new().extract(&MediaBridgeRequest::new(path, MediaKind::Image));

    assert_eq!(outcome.status, MediaBridgeStatus::MetadataOnly);
    assert_eq!(
        outcome.metadata.get("format").map(String::as_str),
        Some("jpg")
    );
    assert!(!outcome.metadata.contains_key("width"));
}

// The fixture engine is a POSIX shell script.
#[cfg(unix)]
#[test]
fn configured_ocr_and_asr_engines_return_text() {
    for (kind, expected) in [
        (MediaKind::Image, "image native text"),
        (MediaKind::Audio, "audio native text"),
    ] {
        let fixture = media_fixture(kind);
        let outcome = runtime_with_engine(kind, "success", Duration::from_secs(1))
            .extract(&MediaBridgeRequest::new(fixture.path, kind));

        assert_eq!(outcome.status, MediaBridgeStatus::Success);
        assert_eq!(outcome.text, expected);
        assert_eq!(
            outcome.metadata.get("text_engine").map(String::as_str),
            Some(if kind == MediaKind::Image {
                "ocr"
            } else {
                "asr"
            })
        );
    }
}

// The fixture engine is a POSIX shell script.
#[cfg(unix)]
#[test]
fn configured_ocr_and_asr_engine_errors_are_reported() {
    for kind in [MediaKind::Image, MediaKind::Audio] {
        let fixture = media_fixture(kind);
        let outcome = runtime_with_engine(kind, "error", Duration::from_secs(1))
            .extract(&MediaBridgeRequest::new(fixture.path, kind));

        assert_eq!(outcome.status, MediaBridgeStatus::Failed);
        assert!(outcome.text.is_empty());
        assert_eq!(outcome.error, "fixture engine failure");
    }
}

// The fixture engine is a POSIX shell script.
#[cfg(unix)]
#[test]
fn configured_ocr_and_asr_engine_timeouts_are_bounded() {
    for kind in [MediaKind::Image, MediaKind::Audio] {
        let fixture = media_fixture(kind);
        let started = Instant::now();
        let outcome = runtime_with_engine(kind, "timeout", Duration::from_millis(25))
            .extract(&MediaBridgeRequest::new(fixture.path, kind));

        assert_eq!(outcome.status, MediaBridgeStatus::Timeout);
        assert!(outcome.text.is_empty());
        assert!(outcome.error.contains("timed out"));
        assert!(started.elapsed() < Duration::from_millis(500));
    }
}

#[cfg(unix)]
struct MediaFixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

#[cfg(unix)]
fn media_fixture(kind: MediaKind) -> MediaFixture {
    let dir = tempdir().expect("tempdir");
    let path = match kind {
        MediaKind::Image => dir.path().join("sample.png"),
        MediaKind::Audio | MediaKind::Video => dir.path().join("sample.wav"),
    };
    fs::write(&path, b"fixture media bytes").expect("write fixture");
    MediaFixture { _dir: dir, path }
}

#[cfg(unix)]
fn runtime_with_engine(kind: MediaKind, mode: &str, timeout: Duration) -> BridgeRuntime {
    let script = match mode {
        "success" => "printf '%s native text\\n' \"$JIKJI_MEDIA_ENGINE_KIND\"",
        "error" => "printf 'fixture engine failure\\n' >&2; exit 23",
        "timeout" => "sleep 1 & wait",
        _ => unreachable!("unknown fixture mode"),
    };
    let engine = NativeEngineConfig::new(PathBuf::from("/bin/sh"))
        .with_args(vec![
            "-c".to_owned(),
            script.to_owned(),
            "fixture-engine".to_owned(),
        ])
        .with_timeout(timeout);
    let mut config = BridgeConfig::default();
    match kind {
        MediaKind::Image => config.ocr = Some(engine),
        MediaKind::Audio | MediaKind::Video => config.asr = Some(engine),
    }
    BridgeRuntime::with_config(config)
}
fn write_png(path: &Path, width: u32, height: u32) {
    let mut bytes = Vec::from(&b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR"[..]);
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend(b"\x08\x02\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00IEND\xaeB`\x82");
    fs::write(path, bytes).expect("write png");
}

fn write_wav(path: &Path, sample_rate: u32, channels: u16, bits: u16, data_bytes: u32) {
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits) / 8;
    let block_align = channels * bits / 8;
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36 + data_bytes).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(channels.to_le_bytes());
    bytes.extend(sample_rate.to_le_bytes());
    bytes.extend(byte_rate.to_le_bytes());
    bytes.extend(block_align.to_le_bytes());
    bytes.extend(bits.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(data_bytes.to_le_bytes());
    bytes.resize(bytes.len() + data_bytes as usize, 0);
    fs::write(path, bytes).expect("write wav");
}
