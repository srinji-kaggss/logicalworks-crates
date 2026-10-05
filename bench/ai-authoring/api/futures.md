# API sheet — the ecosystem standard: the `futures` crate

You may use only the items on this sheet plus `std` and `ai_task_support`.

This is the surface most Rust async code reaches for when it needs bounded
concurrency: combinators over `Stream`s and `Future`s from the `futures` crate,
with a runtime and timer from `lgwks_bot::rt`. Every item below is a real public
item. Signatures are given as declared, trimmed to what a task needs.

## Entering async

- `lgwks_bot::rt::runtime::block_on<F: Future>(future: F) -> F::Output` — build a
  current-thread runtime, drive one future, tear it down.

## Time

- `lgwks_bot::rt::time::sleep(duration: std::time::Duration)` — `async fn`.
- `lgwks_bot::rt::time::timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, Elapsed>`
  Dropping the returned future, or letting it elapse, drops `future`.

## Streams — `futures::stream`

- `futures::stream::iter<I: IntoIterator>(i: I) -> impl Stream<Item = I::Item>`
- `futures::StreamExt` (trait, in scope with `use futures::StreamExt;`)
  - `fn map<F, T>(self, f: F) -> impl Stream<Item = T>`
  - `fn buffered(self, n: usize) -> impl Stream` where `Self::Item: Future` —
    runs at most `n` of the stream's futures at once and yields their outputs in
    **input order**. A finished future's slot is freed when its output is yielded.
  - `fn buffer_unordered(self, n: usize) -> impl Stream` — the same bound, but
    yields outputs in **completion order**.
  - `fn for_each_concurrent<Fut, F>(self, limit: impl Into<Option<usize>>, f: F) -> impl Future<Output = ()>`
  - `fn next(&mut self) -> impl Future<Output = Option<Self::Item>>`
  - `fn collect<C>(self) -> impl Future<Output = C>`
- `futures::TryStreamExt` (trait, `use futures::TryStreamExt;`) — for a stream of
  `Result<T, E>`:
  - `fn try_collect<C>(self) -> impl Future<Output = Result<C, E>>` — stops at the
    first `Err` and drops the stream.
  - `fn try_fold<T, F, Fut>(self, init: T, f: F) -> impl Future<Output = Result<T, E>>`
    where `F: FnMut(T, Self::Ok) -> Fut, Fut: Future<Output = Result<T, E>>`
  - `fn try_for_each_concurrent<Fut, F>(self, limit: impl Into<Option<usize>>, f: F) -> impl Future<Output = Result<(), E>>`

Dropping a stream, or a future built from one, drops every future it was running.

## Futures — `futures::future`

- `futures::future::try_join<A, B>(a: A, b: B) -> impl Future<Output = Result<(A::Ok, B::Ok), E>>`
  Runs both; on the first `Err` returns it and drops the other.
- `futures::try_join!(a, b, ...)` — the macro form.
- `futures::future::join(a, b)`, `futures::future::ready(value)`.
- `futures::FutureExt` (trait): `fn map<T, F: FnOnce(Self::Output) -> T>(self, f: F)`.
- `futures::TryFutureExt` (trait): `fn map_err<E, F: FnOnce(Self::Error) -> E>(self, f: F)`,
  `fn and_then`, `fn try_flatten`.

## A bounded fan-out in this API

```rust
use futures::{StreamExt, TryStreamExt, stream};

fn main() {
    let total: Result<u64, String> = lgwks_bot::rt::runtime::block_on(async {
        let doubled = stream::iter(0u64..100)
            .map(|value| async move { Ok::<u64, String>(value * 2) })
            .buffered(8)
            .try_fold(0u64, |sum, value| async move { Ok(sum + value) });
        doubled.await
    });
    assert_eq!(total, Ok(9900), "0..100 doubled");
}
```

## Notes

- There is no `spawn`. Everything runs on the task that awaits it, so dropping
  the future you return stops all of it.
- `buffered` and `buffer_unordered` take the bound as the argument; `0` would
  never make progress.
