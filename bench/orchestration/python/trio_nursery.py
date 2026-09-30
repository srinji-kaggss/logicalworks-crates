"""Python Trio, the origin of structured concurrency: nursery + Semaphore.

python3 trio_nursery.py <scenario>   (needs trio, pinned in requirements.txt)
"""

import sys
import time

import trio

from site_model import TENANTS, Permanent, Site, Spec, Transient, first_error, report


# BEGIN trio
async def one(site, tenant, item):
    with site.enter() as live:
        key = f"{tenant}/{item}"
        attempt = 1
        while True:
            try:
                with trio.fail_after(site.spec.deadline):
                    value = await site.attempt(key, item)
                break
            except (Transient, trio.TooSlowError):
                if attempt >= site.spec.attempts:
                    raise
                attempt += 1
                await trio.sleep(site.spec.wait)
        live.finish()
        return value


async def run_items(site, tenant, items):
    permits = trio.Semaphore(site.spec.bound)
    values = []

    async def guarded(item):
        try:
            values.append(await one(site, tenant, item))
        finally:
            permits.release()

    async with trio.open_nursery() as nursery:
        for item in items:
            await permits.acquire()
            nursery.start_soon(guarded, item)
    return sum(values)
# END trio


async def main(scenario):
    site = Site(Spec(scenario), trio.sleep)
    spec = site.spec
    totals, error = [], ""
    started = time.perf_counter()

    async def tenant_run(tenant):
        totals.append(await run_items(site, tenant, list(range(spec.items))))

    try:
        async with trio.open_nursery() as tenants:
            for tenant in TENANTS[: spec.tenants]:
                tenants.start_soon(tenant_run, tenant)
            if spec.cancel_after is not None:
                await trio.sleep(spec.cancel_after)
                tenants.cancel_scope.cancel()
    except (Permanent, Transient, trio.TooSlowError, ExceptionGroup) as failure:
        error = first_error(failure)
    if not error and len(totals) < spec.tenants:
        error = "cancelled"
    elapsed = time.perf_counter() - started
    live_at_return, finished_at_return = site.live, site.finished
    await trio.sleep(0.2)
    report("python-trio", site, elapsed, sum(totals), error, live_at_return, finished_at_return, site.live)


if __name__ == "__main__":
    trio.run(main, sys.argv[1] if len(sys.argv) > 1 else "throughput")
