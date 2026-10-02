//! Public-consumer regression checks for codec coordinate contracts.
#![forbid(unsafe_code)]

#[path = "support/consumer_probe.rs"]
mod consumer_probe;

#[cfg(all(feature = "hash", feature = "random"))]
use lgwks_std::hash::Digest;
#[cfg(all(feature = "hash", feature = "random"))]
use lgwks_std::id::Uuid;
#[cfg(all(feature = "random", feature = "hash"))]
use lgwks_std::leb128;
use lgwks_std::{encoding::percent, hex};

#[test]
fn consumer_sees_source_and_decoded_coordinate_spaces() {
    assert_eq!(
        percent::decode("ok%2G"),
        Err(percent::DecodeError::NotHexDigit { at: 4 }),
        "malformed percent escapes identify the original offending byte"
    );
    assert_eq!(
        percent::decode("é%G0"),
        Err(percent::DecodeError::NotHexDigit { at: 3 }),
        "source offsets count UTF-8 input bytes"
    );
    assert_eq!(
        percent::decode("a%20%FF"),
        Err(percent::DecodeError::NotUtf8 { decoded_at: 2 }),
        "UTF-8 failures identify their decoded-byte coordinate"
    );
}

#[test]
fn consumer_uses_atomic_exact_length_hex_decode() {
    let mut output = [0xa5; 2];
    assert_eq!(
        hex::decode_into("aabz", &mut output),
        Err(hex::DecodeError::NotHexDigit { at: 3, byte: b'z' }),
        "invalid data identifies the offending character"
    );
    assert_eq!(output, [0xa5; 2], "refused input leaves output unchanged");
    assert_eq!(
        hex::decode("Af09").as_deref(),
        Ok(&[0xaf, 0x09][..]),
        "the owned convenience API shares uppercase decoding behavior"
    );
}

#[test]
#[cfg(all(feature = "random", feature = "hash"))]
fn consumer_preserves_uuid_digest_and_minimal_leb_profiles()
-> Result<(), Box<dyn std::error::Error>> {
    let uuid = Uuid::parse("00000000-0000-0000-0000-000000000000")?;
    assert_eq!(
        uuid.to_string(),
        "00000000-0000-0000-0000-000000000000",
        "UUID parse/display preserve arbitrary UUID bits"
    );

    let digest =
        Digest::from_hex("af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262")?;
    assert_eq!(
        digest.to_string(),
        digest.to_hex(),
        "digest formatting writes canonical lowercase hex"
    );

    assert_eq!(
        leb128::decode_u64(&[0x00, 0xff]),
        Ok((0, 1)),
        "LEB decoding consumes only the minimal value prefix"
    );
    Ok(())
}

/// A downstream binary with a counting global allocator observes the
/// allocation contract of the fixed-size codec paths. The allocator needs
/// `unsafe`, which this workspace forbids, so it lives in a standalone package.
#[test]
fn consumer_counts_no_allocation_on_fixed_size_codec_paths()
-> Result<(), Box<dyn std::error::Error>> {
    let manifest = format!(
        "[package]\nname = \"alloc_probe\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nlgwks_std = {{ path = {:?}, default-features = false, features = [\"hash\", \"random\"] }}\n",
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
    );
    let stdout = crate::consumer_probe::build_and_run(&manifest, ALLOCATION_PROBE)?;
    for expected in [
        "uuid-parse 0",
        "uuid-display 0",
        "digest-parse 0",
        "digest-display 0",
        "hex-decode-into 0",
        "percent-decode-64-escapes 1",
    ] {
        assert!(
            stdout.lines().any(|line| line == expected),
            "missing `{expected}` in probe output:\n{stdout}"
        );
    }
    Ok(())
}

/// Counts every allocation, reallocation and zeroed allocation in the probe
/// process; each measured region reports the delta across one call.
const ALLOCATION_PROBE: &str = r#"
use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// A stack buffer that refuses to grow, so formatting cannot hide a heap write.
struct Fixed { bytes: [u8; 128], len: usize }
impl std::fmt::Write for Fixed {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let end = self.len.checked_add(text.len()).ok_or(std::fmt::Error)?;
        self.bytes.get_mut(self.len..end).ok_or(std::fmt::Error)?.copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}

fn counted<T>(label: &str, work: impl FnOnce() -> T) -> T {
    let before = ALLOCATIONS.load(Ordering::SeqCst);
    let value = std::hint::black_box(work());
    let after = ALLOCATIONS.load(Ordering::SeqCst);
    println!("{label} {}", after - before);
    value
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let uuid_text = "a1b2c3d4-E5F6-4789-8abc-DEF012345678";
    let digest_text = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
    let percent_text = "%41%42".repeat(32);

    let uuid = counted("uuid-parse", || lgwks_std::id::Uuid::parse(uuid_text))?;
    let mut buffer = Fixed { bytes: [0; 128], len: 0 };
    counted("uuid-display", || write!(buffer, "{uuid}"))?;
    assert_eq!(&buffer.bytes[..buffer.len], uuid_text.to_ascii_lowercase().as_bytes());

    let digest = counted("digest-parse", || lgwks_std::hash::Digest::from_hex(digest_text))?;
    let mut buffer = Fixed { bytes: [0; 128], len: 0 };
    counted("digest-display", || write!(buffer, "{digest}"))?;
    assert_eq!(&buffer.bytes[..buffer.len], digest_text.as_bytes());

    let mut raw = [0u8; 32];
    counted("hex-decode-into", || lgwks_std::hex::decode_into(digest_text, &mut raw))?;
    assert_eq!(&raw, digest.as_bytes());

    let decoded = counted("percent-decode-64-escapes", || lgwks_std::encoding::percent::decode(&percent_text))?;
    assert_eq!(decoded, "AB".repeat(32));
    Ok(())
}
"#;
