# The GPU-accelerated runtime

Status: the deterministic residency model is implemented (`src/runtime.rs`),
and the first backend consumes it: the native GPUI host (`src/native.rs`,
feature `ui`). No wgpu edge, driver capture or shared compute/render residency
exists. The model owns the contract every backend must satisfy (R16, J13).

The native GPUI host that consumes this model is described in [native-host.md](native-host.md).

## Why a model before a backend

Drishti's architecture already separates "reserve before proportional
allocation or submission" from the presentation foundation. GPUI is the
selected foundation, and an accelerated wgpu/WebGPU path is a preferred
candidate rather than proof of universal fallback. Before admitting a GPU
dependency, the framework owes an honest answer to three questions that do not
need a driver to answer:

1. What work is actually demanded, and what is merely allocated?
2. What is uploaded, what is reused, and how many bytes move?
3. What happens when the device is lost or must drain, and how is stale state
   invalidated?

`runtime::Runtime` answers those questions deterministically over the ink lab,
so a backend can be measured against a fixed contract instead of being trusted
because a window appeared.

## Demand, not allocation

`Runtime::present(dt_ms)` asks the [`InkEngine`](crate::ink_lab) for the frame's
demand. If that set is empty, the call returns an idle report, submits nothing
and uploads nothing. Visibility alone does not authorize work; a paused engine
produces no demand. This is the "no demand: sleep" rule made executable.

## Budgets are enforced

A profile declares finite limits — `Budgets` names the maximum shapes per
frame, uploaded bytes per frame, in-flight hosts and admitted hosts. When a
frame would exceed a budget, `present` returns `RuntimeError::Backpressure` and
**commits nothing**: no cache entry, no transfer counter, no frame counter. The
refusal is a typed error, never a silent drop of the overflow. R08 requires
finite enforced limits and safe saturation behaviour; this is where a runtime
profile proves it.

The reference profile (`Budgets::conservative`) is deliberately small — enough
for a dock's ink chrome, not a hundred animated tiles. Numeric calibration
against a real device remains open (G07).

## Reuse and transfer accounting

Each frame carries a deterministic `digest` from the ink lab. The runtime caches
the last uploaded digest per host:

* an unchanged digest produces a receipt with `upload_bytes == 0` and counts as
  a reuse, so saved bandwidth is a measured number; and
* a changed digest uploads an estimate (`BYTES_PER_SHAPE` per shape plus filter
  bytes) and updates the cache.

The per-shape and per-filter figures are declared estimates, not measured
driver costs. They exist so a profile can be calibrated; the honest statement is
that they are an accounting model, not a capture. A real residency proof must
compare them with a device capture (G03).

## Device epochs and loss

The device has three states: `Ready`, `Draining`, and `Lost`. `lose_device`
clears the transfer cache and refuses frames with
`RuntimeError::DeviceUnavailable`; `drain` stops new submissions while admitted
work finishes. Recovery is not a silent retry: `recover` bumps the device
epoch, which invalidates every prior submission and forces the next frame to
upload afresh.

The epoch is the same rule J13 asks a real capture to demonstrate — a result
computed on device epoch *n* may not be adopted or reused on epoch *n+1* without
new evidence.

## What is not here

* **No shared device/queue/allocator.** GPUI using wgpu somewhere does not
  establish one shareable residency contract for compute and render. That proof
  (G03) remains blocked; this module does not claim it.
* **No driver or adapter.** `wgpu` is an optional registered feature admitted
  through `lgwks_deps`, not a direct dependency of this package.
* **No performance claim.** There is no p50/p95/p99, no peak RSS, and no
  comparison against a competent baseline; the assurance contract forbids
  inventing them.

## API

```text
Runtime::new(seed, Budgets)
runtime.add_host(HostSpec) -> Result<HostId, RuntimeError>
runtime.set_visible(id, bool)
runtime.set_input(id, Input) -> Result<(), RuntimeError>
runtime.set_options(id, Options) -> Result<(), RuntimeError>
runtime.set_quality(Quality) · set_reduce_motion(bool) · set_paused(bool)
runtime.present(dt_ms) -> Result<RuntimeReport, RuntimeError>
report.receipts() -> &[FrameReceipt] · report.idle · report.epoch
report.uploaded_bytes · report.reused · report.uploads() -> &[Render]
runtime.lose_device() · drain() · recover() -> epoch
runtime.totals() -> RuntimeTotals · runtime.ink_stats() -> Stats
```

`RuntimeError` is typed: `DeviceUnavailable`, `Backpressure`, `HostLimit`, and
`Rejected(InkError)` for a malformed delta, input or option (see
[the ink lab](crate::ink_lab) for the accepted domains). A rejected call
commits nothing. A caller must handle each; there is no swallowing path.
