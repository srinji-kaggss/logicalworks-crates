# Task: `tenant-cache`

Write a Rust library crate whose crate root (`src/lib.rs`) exposes exactly these
public items:

```rust
#[derive(Debug, PartialEq, Eq)]
pub enum CacheError {
    Refused,
}

pub async fn solve(tenant: &str, resource: &str, token: &str) -> Result<Vec<u8>, CacheError>
```

`solve` serves one cached value per `(tenant, resource)` and must satisfy this
contract:

- The value for `(tenant, resource)` is exactly the bytes of
  `format!("value-of-{tenant}-{resource}")`. The same `(tenant, resource)`
  resolves to the same bytes on every call.
- Authority is per tenant: the token for `tenant` is exactly
  `format!("token-for-{tenant}")`. A call whose token is not the requested
  tenant's token — a wrong token, or another tenant's token — resolves with
  `Err(CacheError::Refused)` and records, caches and returns nothing for that
  call.
- Isolation is by tenant: two tenants asking for the same `resource` each get
  their own tenant's value. One tenant's cached value is never observable from
  another tenant, however the calls interleave, up to 1,000 in flight at once.
- If the caller drops the returned future mid-run, later calls still resolve
  correctly: a dropped call poisons nothing it shares with the others.

You may use only the items on the API sheet that accompanies this task, plus
`std` and `ai_task_support`. Do not use any crate that is not on the sheet.

Reply with exactly one fenced ```rust block. The block must be the complete
contents of `src/lib.rs` and nothing else — no prose outside the block.
