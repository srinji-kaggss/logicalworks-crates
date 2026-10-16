# 4.7 Portable — same semantics on all declared targets

Part of [production readiness](../production-readiness.md), §4, nine axes.

**⚠️ — one operating system is executed, and eight targets are built. A build
receipt is not an execution receipt, and this section says which is which.**

Twenty-two hosted job definitions in `.github/workflows/ci.yml`, the
twenty-one at `977c79d4` plus #276's `target-matrix`. Their `runs-on`
distribution is **18 `ubuntu-latest`, 2 `macos-14` (`gpui-macos`,
`candle-macos`), 1 `windows-latest` (`gpui-windows`), 1 `matrix.os`**, and that
single `matrix.os` job is `appcui-native` — so "three operating systems" is
true of those storefront features and not of the estate. The `lgwks_std` /
`lgwks_bot` / `lgwks_ast` test and clippy lanes are `ubuntu-latest` only. A
`wasm32-wasip1` boundary lane checks that the default feature set compiles for
WASI — a build check, not an executed containment test. `os.uname` and other
host-only calls are behind `platform` dispatch.

**The executed target matrix (#276).** `scripts/check-target-matrix.sh` is a
required lane that runs `cargo check --locked --target <t>` per declared target
and per crate. On `aarch64-apple-darwin` (rustc 1.99.0), 42 checks in 69 s from
an empty target directory:

| Target | `lgwks_std` | `lgwks_std` `--no-default-features` | `lgwks_std` `--features random` | `lgwks_deps` | `lgwks_std` `--features full` | `lgwks_ast` |
|---|---|---|---|---|---|---|
| `aarch64-apple-darwin` (host) | builds | builds | builds | builds | builds | builds |
| `aarch64-unknown-linux-gnu` | builds | builds | builds | builds | **not exercised** | **not exercised** |
| `x86_64-unknown-linux-musl` | builds | builds | builds | builds | **not exercised** | **not exercised** |
| `wasm32-wasip1` | builds | builds | builds | builds | builds | **not exercised** |
| `x86_64-unknown-freebsd` | builds | builds | builds | builds | **not exercised** | **not exercised** |
| `aarch64-linux-android` | builds | builds | builds | builds | **not exercised** | **not exercised** |
| `aarch64-apple-ios` | builds | builds | builds | builds | builds | builds |

**"Builds" is a build receipt and "not exercised" is a named gap, not a pass.**
The nine not-exercised cells each fail with the exact error
`failed to find tool "aarch64-linux-gnu-gcc"` (or the musl, Android, FreeBSD or
`clang` equivalent): `ring`, reached through `http`'s rustls, and the
tree-sitter grammars under `lgwks_ast` compile C per target, and this runner has
no cross C toolchain for them. The lane accepts that one failure reason by name
and fails on any other, so the exemption cannot become a lid. **Support is
therefore claimed for `lgwks_std` on seven targets with the `full` feature set
only where the row says `builds`, and for `lgwks_ast` and `lgwks_deps` on the
targets where their row says `builds` — never for a target that was not
exercised.**

`random` was the module that made the gap load-bearing: it carried a
three-target `compile_error!` while the backend it wraps supported dozens more,
so `lgwks_std` with `random` or `http` could not be built for iOS, Android, a
BSD or WASI at all. It now holds no target list of its own, and its typed
`EntropyError` is what a caller on any of these targets gets when the source
fails.

**Containment per OS, as executed evidence (#263, #337).** On Unix the supervisor
captures the leader's descendants and stops each by pid as well as signalling
the group, so a `setsid` escapee is stopped: `tests/it/process_escape.rs`
observes it gone with `kill -0`, 4 tests × 1,000 iterations with zero failures
on `aarch64-apple-darwin` (4,000 executions, `--retries 0`, `leaky` only where
the test intends it). On Linux the tree additionally runs in its own cgroup v2
scope, killed first with `cgroup.kill`, and the supervisor adopts orphans
through the child-subreaper flag: `sim_confinement ::
a_cgroup_scope_kills_a_setsid_escapee_where_the_mount_allows` passes with the
receipt naming `CgroupKill` where the mount admits a scope (privileged
`rust:1.99.0-bookworm` container, `aarch64`), and falls back to the capture
rounds where it does not; `sim_kernel_owners ::
an_adopted_orphan_is_named_by_its_adoption` proves the attribution (a pid that
is both captured and adopted is signalled, then reaped), and the sweep in
`start` / `confirm_absence` collects the adoptees no guard captured, so a
killed descendant is never left as a `kill -0`-present zombie. The fire test
drives 1,000 supervised depth-3 trees under cancellation at in-flight 100 (ten
rounds of one hundred 7-process trees, 7,000 processes) and at in-flight 1,000
(one round of one thousand 3-process chains, 3,000 processes — the host's
`maxprocperuid` of 4,000 forbids ten thousand processes at once, so the second
level uses chains rather than fan-out): zero surviving descendants in every
round, verified from the OS process table. Cleanup latency p50/p99 559/574 ms
at in-flight 100 and 6,582 ms at in-flight 1,000 (one thousand macOS `ps`
snapshots dominate the second number), peak RSS 13.8 MB and 58.8 MB
respectively. A descendant orphaned to init before the cleanup ran, where no
subreaper adopted it, is still not reachable from the leader: a cleanup that
begins after the leader exited on its own reads no table and reports
`ResidualRisk::LeaderExited`, so its containment is never `is_complete()`
(INV-BOT-158, #347). macOS lanes additionally confine through
`ProcessSpec::confinement` (`sim_confinement ::
a_sandboxed_child_is_denied_its_denied_subtree`): a denied write fails with the
kernel's refusal and leaves no file, while a confined `true` still exits zero.
The sandbox costs one extra fork and exec per spawn, measured on this host at
p50 10.80 ms against 3.17 ms for a bare `true` (n=200 each); the in-process
`sandbox_init` that would avoid it needs an FFI leaf the workspace gate
refuses, and Linux subtree-deny needs Landlock syscall bindings the estate has
not admitted — both recorded, neither shipped. On Windows (#263) each supervised
process joins a job object as it starts, and cancel, deadline, exit and drop
terminate the whole job: the receipt is `CleanupConfirmed` with
`ContainmentMechanism::JobObject` only when the kernel reads the job's membership
empty, and a spec that captures a stream is refused before the fork. The calls
are fourteen `kernel32` externs in `lgwks_std::process::windows`, the one Windows
`unsafe` leaf (an exception the guard's register names), with no Windows API
crate. The settle decision table runs on every host against a scripted job (7
tests) and the Windows target type-checks for both crates, tests included;
`tests/it/process_job.rs` drives a real job but no Windows host has run it,
because no CI job runs the `lgwks_std` or `lgwks_bot` suites on `windows-latest`
(adding one needs the `workflow` token scope). Windows containment is therefore a
code, type-check and scripted-decision receipt, not yet an executed one.

**The feature × OS × backend × assurance matrix, and what it does not say.** The
bot's supervised process backend returns `Unsupported` off Unix and Windows,
and a green `windows-latest` job is still a *build* receipt for `lgwks_std` and
`lgwks_bot`, never a *containment* receipt: no Windows run in CI has stopped a
job object. A build check and an executed containment test are different facts, and
only the second one is a portability claim about the runtime. The eight-target
matrix above is itself a build receipt: every row was executed by `cargo check`
and none of them ran a test.

*Not covered:* 32-bit, any target outside the eight rows, and any executed test
on a non-hosted-runner target. The performance numbers in §4.9 are from one
machine and are explicitly not a cross-platform claim.
