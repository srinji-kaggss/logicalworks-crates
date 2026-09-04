//! `task` owns synchronous future execution for CLI, DAG interpreters, and
//! test runners, enforcing INV-TASK-ZERO-RUNTIME: futures are driven to
//! completion on the current thread using `std::task::Wake` and OS thread parking,
//! with zero background reactors, zero worker threadpools, and zero external
//! dependencies.
//!
//! Replaces the need for pulling in `tokio` or `pollster` when all an application
//! needs is to synchronously await a `Future` or evaluate a DAG pipeline.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

struct ThreadWaker {
    thread: Thread,
    notified: AtomicBool,
}

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.notified.store(true, Ordering::Release);
        self.thread.unpark();
    }
}

/// Synchronously polls a `Future` to completion on the current thread.
///
/// If the future is not immediately ready, the current thread is parked until
/// woken by the future's waker.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let signal = Arc::new(ThreadWaker {
        thread: thread::current(),
        notified: AtomicBool::new(false),
    });
    let waker = Waker::from(signal.clone());
    let mut cx = Context::from_waker(&waker);

    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(val) => return val,
            Poll::Pending => {
                while !signal.notified.swap(false, Ordering::Acquire) {
                    thread::park();
                }
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    #[test]
    fn immediate_future_returns_value() {
        let result = block_on(async { 42 });
        assert_eq!(result, 42);
    }

    #[test]
    fn yields_and_resumes() {
        async fn step() -> String {
            let first_part = async { "hello" }.await;
            let second_part = async { "world" }.await;
            format!("{first_part} {second_part}")
        }

        assert_eq!(block_on(step()), "hello world");
    }

    struct ChannelFuture {
        rx: std::sync::mpsc::Receiver<i32>,
        waker_slot: Arc<Mutex<Option<Waker>>>,
    }

    impl Future for ChannelFuture {
        type Output = i32;
        fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            match self.rx.try_recv() {
                Ok(val) => Poll::Ready(val),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    *self.waker_slot.lock().unwrap() = Some(cx.waker().clone());
                    Poll::Pending
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => panic!("disconnected"),
            }
        }
    }

    #[test]
    fn threaded_waker_unparks() {
        let (tx, rx) = std::sync::mpsc::channel();
        let handle: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
        let handle_clone = handle.clone();

        thread::spawn(move || {
            thread::sleep(Duration::from_millis(5));
            tx.send(100).unwrap();
            let mut slot = handle_clone.lock().unwrap();
            if let Some(waker) = slot.take() {
                waker.wake();
            }
        });

        let res = block_on(ChannelFuture {
            rx,
            waker_slot: handle,
        });

        assert_eq!(res, 100);
    }
}
