# lgwks_macros

The `script!` orchestration language for `lgwks_bot`. Use it as
`lgwks_bot::script!`; this crate is the syntax, and `lgwks_bot::script` is the
runtime every block expands into.

```text
lgwks_bot::script! {
    pub flow crawl(site: &Site, paths: Vec<String>) -> Vec<usize>:
        let pages = each path in paths, at most 16 at once:
            retry up to 3 times, waiting 200ms:
                within 5s:
                    site.fetch(scope.key(), &path).await.or_retry()?
        give back pages.iter().map(|page| page.len()).collect()
}
```

Every fan-out is bounded, every wait has a deadline, every retry keeps one
idempotency key, every step is keyed by tenant and path, and every script emits
an `ARCHITECTURE` map compiled from the same tokens. See the crate
documentation for the full language and the list of what it refuses.
