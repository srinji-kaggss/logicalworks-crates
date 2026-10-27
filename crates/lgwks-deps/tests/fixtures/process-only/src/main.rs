// Process-only consumer: supervises processes without the ECS substrate,
// the wire or RON encodings, the tracing installer, or the TLS/HTTP stack
// (#687).
//
// `process` implies `io`, `sync`, and `time` (and through them `rt`), but
// never `ecs`, `trace`, `wire`, `ron`, `script`, or `inspect`. A `Bot` import
// here must fail to compile; the supervisor path must not.

fn main() {}
