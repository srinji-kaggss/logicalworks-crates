//! Probe: are the estate's stated anti-slop invariants actually unrepresentable
//! through the `lgwks_bot::rt` facade, or merely re-exported?
#[allow(clippy::unwrap_used)]
fn main() {
    // INVARIANT CLAIM: "unbounded_channel() is forbidden."
    // If this line compiles, the facade re-exports the unbounded constructor.
    let (_tx, _rx) = lgwks_bot::rt::sync::mpsc::unbounded_channel::<u8>();

    // INVARIANT CLAIM: "Zero Un-Tracked Background Tasks."
    // If this compiles, fire-and-forget is representable: the JoinHandle is
    // droppable and nothing records the task.
    let rt = lgwks_bot::Runtime::new().unwrap();
    rt.block_on(async {
        let _dropped_on_the_floor = lgwks_bot::rt::task::spawn(async {});
        drop(_dropped_on_the_floor);
    });

    // INVARIANT CLAIM: "No Mutex across .await" / std Mutex banned in async.
    // If this compiles, the std Mutex is reachable and unprotected.
    let m = std::sync::Mutex::new(0u8);
    rt.block_on(async {
        let _guard = m.lock().unwrap();
        lgwks_bot::rt::task::yield_now().await;
    });
}
