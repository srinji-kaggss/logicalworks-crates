# The native GPUI host

Status: the first backend of the residency model in [gpu-runtime.md](gpu-runtime.md), behind the
`ui` feature (`src/native.rs`); not qualified on a device (G15).

`native::run` opens a window with one tile per effect and treats the runtime as
its only source of frames:

* it calls `Runtime::present` once per GPUI frame and requests another frame
  only while the report has demand. An idle runtime puts the window to sleep
  until input arrives;
* it keeps one *resident* frame per host. It replaces that frame only for
  `report.uploads()`, paints the resident copy for reused receipts, and drops
  every resident frame when the epoch changes or the device is lost;
* backpressure and device loss appear in the status line and in the
  `SessionReport`, with measured frame-interval percentiles and peak counters.

GPUI re-submits its scene each frame, so a reused receipt saves the host's
tessellation and copy work, not a driver upload. `BYTES_PER_SHAPE` remains a
modelled figure until G15's device capture. The goo filter and text
turbulence are not painted, because GPUI 0.2 has no blur or alpha-threshold
pass. Liquid shapes are drawn flat.

