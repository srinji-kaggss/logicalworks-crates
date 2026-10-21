//! The supported gate surface as a downstream consumer compiles it (#526).
//!
//! Unit tests can see `pub(crate)` paths. This module names only public items,
//! so a visibility regression fails here rather than in a user's build.

use std::path::Path;

use lgwks_deps::contract::Contract;
use lgwks_deps::metadata::{Collected, MetadataError};
use lgwks_deps::{CONTRACT_PATH, GateError, Refusal, Verdict, check_verdict, repository_root};

/// Fails to compile when a name leaves the supported surface.
fn supported<T: ?Sized>() {
    let _name = core::any::type_name::<T>();
}

#[test]
fn a_downstream_consumer_takes_the_verdict_and_its_cleanup()
-> Result<(), Box<dyn std::error::Error>> {
    supported::<Contract>();
    supported::<GateError>();
    supported::<MetadataError>();
    supported::<Refusal>();
    supported::<Verdict>();
    supported::<Collected<Verdict>>();

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repository = repository_root(&root)?;
    assert!(
        repository.join(CONTRACT_PATH).is_file(),
        "the register the gate reads is not beside the repository it names"
    );

    let collected: Collected<Verdict> = match check_verdict(&repository, None) {
        Ok(collected) => collected,
        Err(error) => {
            return Err(format!("the repository refused its own register: {error}").into());
        }
    };
    let (verdict, cleanup) = collected.into_parts();
    let _register: &Contract = verdict.register();
    assert!(
        cleanup.is_none(),
        "a complete metadata read leaves no capture behind"
    );
    Ok(())
}
