# Getting started with lgwks_bot: the program

Pages of this guide: [manifest](getting-started.md) · [the program](getting-started-program.md) · [the pieces](getting-started-pieces.md)

## The program

The program is `crates/lgwks-bot/examples/queue_watch.rs`. The part you write is
below: the registry that names the source and the action, the flow, a host that
runs it, and a `main` that runs it three times. The source (`QueueDepth`), the
action (`Page`) and the two counters they read and move are defined above these
lines in the file; [the pieces](getting-started-pieces.md) says what each is.

```rust
domains! {
    /// What the flow below may name: one source and one action.
    QUEUE_DOMAINS {
        observe {
            "queue::depth" => QueueDepth::from_target,
        }
        execute {
            "notify::page" => Page::from_target,
        }
    }
}

lgwks_bot::script! {
    /// Read the depth of the queue called `name`; page on call when it is over ten.
    flow watch(name: &str) -> u32:
        let depth: u32 = observe queue::depth of name
        if depth > 10:
            act notify::page on "oncall" with depth
        give back depth
}

/// A host for the `queue-watch` tenant over [`QUEUE_DOMAINS`], holding `grants`.
fn host(grants: GrantSet) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder("queue-watch")?
        .domains(&QUEUE_DOMAINS)
        .grants(grants)
        .default_deadline(Duration::from_secs(30))
        .build()?)
}

/// Set the queue to `depth` and run the flow once on `host`: the depth it read,
/// or the reason the run was refused.
fn run_at(host: &Host, depth: u32) -> Result<u32, Box<dyn Error>> {
    DEPTH.store(depth, Ordering::SeqCst);
    let watching = task("watching", |scope: Scope, name: &'static str| async move {
        watch(&scope, name).await
    })?;
    let report = lgwks_bot::block_on(host.run(&watching, "jobs"));
    Ok(report.result().copied().map_err(ToString::to_string)?)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut out = std::io::stdout().lock();
    let granted = host(GrantSet::empty().grant(Cap::notify()))?;

    let seen = run_at(&granted, 3)?;
    let pages = PAGES.load(Ordering::SeqCst);
    writeln!(out, "depth {seen}, pages delivered: {pages}")?;
    assert_eq!(pages, 0, "depth 3 is under the threshold");

    let seen = run_at(&granted, 11)?;
    let pages = PAGES.load(Ordering::SeqCst);
    writeln!(out, "depth {seen}, pages delivered: {pages}")?;
    assert_eq!(pages, 1, "one page was delivered");

    let refusal = run_at(&host(GrantSet::empty())?, 11)
        .err()
        .ok_or("a host without the grant ran the page")?;
    let pages = PAGES.load(Ordering::SeqCst);
    writeln!(out, "no grant: {refusal}")?;
    assert_eq!(pages, 1, "a refused page is not delivered");
    Ok(())
}
```

## What it prints

```text
depth 3, pages delivered: 0
depth 11, pages delivered: 1
no grant: watching/watch/page: blocked: 1 ungranted: bot.notify
```

Those three lines are the output of `cargo run -p lgwks_bot --example queue_watch`
on the commit that added the example, and its three assertions hold the page
counts (0, then 1, then still 1). Depth 3 is under ten, so the flow reads the
depth and gives it back. Depth 11 runs the action once. The third run is the same
flow on a host that was not granted `bot.notify`: it is refused at the page step,
named by its path, and the action never runs.
