// Probe example: compiled only with `--features probe` to prove the
// process-only graph carries no deselected capability (#687).
//
// A `Bot` import here must fail: the process-only closure has no ECS
// substrate, no wire or RON encoding, no tracing installer, and no TLS.

fn main() {}
