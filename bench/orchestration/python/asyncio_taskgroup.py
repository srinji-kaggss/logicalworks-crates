"""Python asyncio, written as a careful author would: TaskGroup + Semaphore.

python3 asyncio_taskgroup.py <scenario>
"""

import asyncio
import sys
import time

from site_model import TENANTS, Permanent, Site, Spec, Transient, first_error, report


# BEGIN asyncio
async def one(site, tenant, item):
    with site.enter() as live:
        key = f"{tenant}/{item}"
        attempt = 1
        while True:
            try:
                async with asyncio.timeout(site.spec.deadline):
                    value = await site.attempt(key, item)
                break
            except (Transient, TimeoutError):
                if attempt >= site.spec.attempts:
                    raise
                attempt += 1
                await asyncio.sleep(site.spec.wait)
        live.finish()
        return value


async def run_items(site, tenant, items):
    permits = asyncio.Semaphore(site.spec.bound)

    async def guarded(item):
        try:
            return await one(site, tenant, item)
        finally:
            permits.release()

    tasks = []
    async with asyncio.TaskGroup() as group:
        for item in items:
            await permits.acquire()
            tasks.append(group.create_task(guarded(item)))
    return sum(task.result() for task in tasks)
# END asyncio


async def main(scenario):
    site = Site(Spec(scenario), asyncio.sleep)
    spec = site.spec
    started = time.perf_counter()
    tenants = [
        asyncio.create_task(run_items(site, tenant, list(range(spec.items))))
        for tenant in TENANTS[: spec.tenants]
    ]
    if spec.cancel_after is not None:
        await asyncio.sleep(spec.cancel_after)
        for task in tenants:
            task.cancel()
    total, error = 0, ""
    for task in tenants:
        try:
            total += await task
        except asyncio.CancelledError:
            error = error or "cancelled"
        except (Permanent, Transient, TimeoutError, ExceptionGroup) as failure:
            error = error or first_error(failure)
    elapsed = time.perf_counter() - started
    live_at_return, finished_at_return = site.live, site.finished
    await asyncio.sleep(0.2)
    report("python-asyncio", site, elapsed, total, error, live_at_return, finished_at_return, site.live)


if __name__ == "__main__":
    asyncio.run(main(sys.argv[1] if len(sys.argv) > 1 else "throughput"))
