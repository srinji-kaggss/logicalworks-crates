# lgwks_std reference

Part of the [lgwks_std consumer guide](index.md). Moved from the crate README.

## Feature table

| Feature | Modules | What it adds | External deps |
|---------|---------|-------------|---------------|
| `core` (default) | encoding, fs, glob, hex, leb128, retry, seeded, similarity, task, time | — | **0** |
| `trace` (default) | trace | Structured logging plus default debugger install | tracing, tracing-subscriber stack |
| `random` | random, id | UUID v4, OS entropy | getrandom |
| `hash` | hash | BLAKE3 content-addressable hashing | blake3 |
| `pattern` | pattern | Compiled regex with explicit compile, input and replacement-output limits | regex |
| `json` | json | JSON serialization | serde, serde_json |
| `ron` | ron | Rusty Object Notation | serde, ron |
| `wire` | wire | Zero-copy binary serialization | rkyv |
| `http` | http | Blocking HTTPS client (rustls-only TLS) | ureq, iri-string |
| `online` | online | TCP reachability probing | none |
| `fs-raw` | fs | Bytes available on a filesystem, unprivileged; handle-relative `fs::capability::Dir` for trees being rewritten while you walk them | rustix |
| `process` | process | Signal a whole process group; observe a child's exit without releasing its pid | rustix |
| `full` | all of the above | — | all of the above |

## Module reference

| Module | What it does | Replaces |
|--------|-------------|----------|
| `encoding` | Base64 and percent-encoding | `base64`, `percent-encoding` |
| `fs` | Recursive directory walking for trusted trees, with omissions reported rather than hidden and a best-effort in-root symlink policy; a predicate walk that prunes subtrees unread and returns each entry's `lstat` and root-relative path | `walkdir` (including `filter_entry` pruning) |
| `glob` | Unicode-scalar path-text matching; checked strict compile plus explicit legacy dialect | `glob` (matching only; no directory walking or POSIX shell expansion) |
| `hex` | Hex encode and decode | `hex` |
| `leb128` | LEB128 variable-length integer encoding | — |
| `retry` | Retry budgets: attempts, exponential backoff with caller jitter, deadlines | — |
| `seeded` | A deterministic xoshiro256\*\* stream for simulations: one seed, one sequence on every target, unbiased bounded draws. Not a source of randomness | `rand_xoshiro` + `rand::Rng::gen_range`, hand-rolled splitmix64 |
| `task` | Executor that drives futures on the current thread: `block_on`, interleaved `join_all`, off-thread `spawn_blocking` | — |
| `time` | RFC 3339 timestamps, calendar math | `chrono`, `time` |
| `random` | OS entropy via `getrandom`, typed OS cause | `getrandom` |
| `id` | UUID v4 generation and parsing | `uuid` |
| `hash` | BLAKE3 content-addressable hashing | `blake3` |
| `pattern` | Compiled regex matching: single search is O(m*n); complete greedy iteration may be O(m*n^2). `Regex::with_config` bounds source-pattern bytes, compiled size, nesting, input bytes and replacement output bytes. | `regex` |
| `json` | JSON encoding and decoding via serde | `serde_json` |
| `ron` | RON encoding and decoding via serde | `ron` |
| `wire` | Zero-copy binary wire serialization via rkyv | `rkyv` |
| `http` | Blocking HTTP GET/POST with strict URL validation | `ureq`, `iri-string` |
| `online` | TCP reachability probing, zero-dep | — |
| `similarity` | Edit-distance and cosine scorers, weighted composition, acceptance thresholds | — |
| `process` | Process-group signalling and unreaped exit observation (Unix-only) | — |
| `fs::capability` | Handle-relative `openat`/`statat`/`unlinkat`/`mkdirat` access (Unix-only) | — |

## JSON and RON decoding

JSON and RON text/slice decoders can borrow unescaped string fields from their
input. Escaped strings require decoded storage: use `String` when input may
contain escapes. JSON reader decoding remains owned because the input is read
through a temporary buffer. RON's `to_writer` renders before writing, so a
serialization error leaves the writer untouched, while an I/O error may leave
a prefix. Its return type is `ron::WriterError`, with distinct `Serialize`
and `Write` variants and the original cause available through `Error::source`;
callers that matched `ron::Error` should update those matches. The previous
`write_all failed after {n} bytes` display text is removed because that count
was not observed; the facade defines no serialized error representation.

## Targets and the one entropy source

`random` supports every target `getrandom` supports and holds no target list of
its own, so a target the backend already supported is not refused here. It is
also the only randomness source: a refused read returns `EntropyError` and
substitutes nothing, and the error carries the OS's own code
(`raw_os_error`), a `#[non_exhaustive]` `EntropyErrorKind`, and the portable
`io::ErrorKind` that code maps to. A caller can tell an interruption from a host
with no entropy device instead of reading a message.

On a refusal the buffer passed to `fill_bytes` is **unspecified** — partly
written, wholly written, or untouched — and must not be read. `bytes::N` has no
such window: a failed draw returns no array.

`scripts/check-target-matrix.sh` is the gate lane that runs `cargo check` across
the declared target set (Linux glibc and musl, aarch64 and x86_64, WASI,
FreeBSD, Android, iOS, and the host), so "supported" means a build that was
executed rather than a `cfg` that was read.

## Threat models: which walk to use

`fs::walk_dir` identifies files by path and says so: INV-FS-2 records that its
identity rechecks are best-effort, and the module header repeats it. That is the
right tradeoff for a tree you trust and the wrong one for a directory another
process can rewrite mid-walk.

For that, use `fs::capability::Dir`. It opens a directory once and resolves every
name beneath it from that one descriptor, so a name swapped underneath the walk
cannot redirect it — the failure is `ENOENT` on the entry that changed, which
`walk_dir_tolerant`'s omissions already have a place to record. Its
`entry_names` is Linux-only, because listing a descriptor needs `getdents64` and
the BSDs have no equivalent syscall; every other operation is `*at(2)`. A listing
is bounded by `ListLimits` and returns UTF-8 names only, the name type every
other `Dir` method takes, counting any other name as unaddressable.
