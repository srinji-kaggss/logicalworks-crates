//! Feature-unification test for the wire format descriptor.
//!
//! rkyv selects its archived primitive format from Cargo features, not from
//! the host, and those features are unified across the whole build graph. A
//! single crate cannot observe two configurations in one process, so this test
//! builds a *separate* minimal consumer crate that selects
//! `rkyv/pointer_width_16`, `rkyv/big_endian` and `rkyv/unaligned` and runs it.
//! It then asserts what that alternate build reports and emits, next to what
//! this build reports and emits: the descriptor must move with the
//! configuration, and the same schema must produce different bytes under a
//! different pointer width.
//!
//! This is the acceptance proof for #167 W1/A3, and the reason the facade
//! reports the actual format rather than asserting a fixed one.
#![forbid(unsafe_code)]

#[cfg(feature = "wire")]
use crate::consumer_probe;
#[cfg(feature = "wire")]
use crate::wire_record;

#[cfg(feature = "wire")]
mod probe {
    use lgwks_std::wire::format_descriptor;

    use crate::wire_feature_unification::wire_record::{PINNED_POINTER_WIDTH_BITS, fixture_hex};

    /// A minimal consumer that selects `pointer_width_16` and reports what it
    /// observes. Kept byte-identical in schema to the retained fixture.
    const PROBE_MAIN: &str = r#"
use std::collections::BTreeMap;
use lgwks_std::wire::{Archive, Serialize, WireError, format_descriptor, to_bytes};

#[derive(Archive, Serialize, Debug)]
#[rkyv(crate = lgwks_std::wire::rkyv, derive(Debug))]
struct ConsumerChild { value: u64 }

#[derive(Archive, Serialize, Debug)]
#[rkyv(crate = lgwks_std::wire::rkyv, derive(Debug))]
struct ConsumerRecord {
    name: String,
    indices: Vec<usize>,
    signed_index: isize,
    fields: BTreeMap<String, u32>,
    child: Option<Box<ConsumerChild>>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let format = format_descriptor()?;
    println!("pointer-width {}", format.pointer_width_bits);
    println!("endianness {:?}", format.endianness);
    println!("u32-align {}", format.archived_u32_alignment);
    let mut fields = BTreeMap::new();
    fields.insert(String::from("height"), 200);
    fields.insert(String::from("width"), 100);
    let value = ConsumerRecord {
        name: String::from("consumer fixture"),
        indices: vec![3, 5, 8],
        signed_index: -13,
        fields,
        child: Some(Box::new(ConsumerChild { value: 42 })),
    };
    let bytes = to_bytes::<WireError>(&value)?;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    println!("bytes {hex}");
    Ok(())
}
"#;

    #[test]
    fn alternate_width_endianness_and_alignment_change_the_reported_format()
    -> Result<(), Box<dyn std::error::Error>> {
        let host = format_descriptor()?;
        assert_eq!(
            host.pointer_width_bits, PINNED_POINTER_WIDTH_BITS,
            "the default build's descriptor matches the retained fixture's pinned width"
        );

        let manifest = format!(
            "[package]\nname = \"wire_unification_probe\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nlgwks_std = {{ path = {:?}, default-features = false, features = [\"wire\"] }}\nrkyv = {{ version = \"0.8\", default-features = false, features = [\"alloc\", \"bytecheck\", \"pointer_width_16\", \"big_endian\", \"unaligned\"] }}\n",
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        );
        let stdout =
            crate::wire_feature_unification::consumer_probe::build_and_run(&manifest, PROBE_MAIN)?;

        assert!(
            stdout.lines().any(|line| line == "pointer-width 16"),
            "the pointer_width_16 build must report width 16, got:\n{stdout}"
        );
        assert!(
            stdout.lines().any(|line| line == "endianness Big"),
            "the big_endian build must report big-endian byte order, got:\n{stdout}"
        );
        assert!(
            stdout.lines().any(|line| line == "u32-align 1"),
            "the unaligned build must report an alignment of one byte, got:\n{stdout}"
        );
        let emitted = stdout
            .lines()
            .find_map(|line| line.strip_prefix("bytes "))
            .ok_or("probe did not report its serialized bytes")?;
        assert_ne!(
            emitted,
            fixture_hex(),
            "a different pointer width must not emit the retained 32-bit fixture bytes"
        );
        Ok(())
    }
}
