---
name: asyncio
description: Use when debugging event loop hangs, task scheduling issues, call_soon vs call_soon_threadsafe confusion, Future callback timing, _enter_task/_leave_task conflicts, uvloop compatibility problems, or sniffio/anyio backend detection failures. Also use when verifying asyncio assumptions via quick Python one-liners.
---

# asyncio Internals Reference

CPython 3.11 reference. Behavior may differ on 3.12+ (eager task factory) and 3.13+ (free-threaded).

## The Event Loop Cycle: `_run_once`

Every `run_forever()` call loops over `_run_once()`. One iteration:

```
1. Process _scheduled heap  (timers due → move to _ready)
2. Poll I/O via selector     (select/epoll/kqueue with timeout)
3. Process _ready deque      (callbacks, exactly ntodo items)
```

**Source:** [`Lib/asyncio/base_events.py:BaseEventLoop._run_once`](https://github.com/python/cpython/blob/v3.11.14/Lib/asyncio/base_events.py#L1845)

### Critical detail: `ntodo` snapshot

```python
# From CPython 3.11 base_events.py _run_once:
ntodo = len(self._ready)
for i in range(ntodo):
    handle = self._ready.popleft()
    if handle._cancelled:
        continue
    handle._run()
```

Callbacks added to `_ready` **during** this loop are NOT processed until the **next** `_run_once` cycle. This means a callback that schedules another callback requires two full cycles.

### Timeout selection

```python
if self._ready or self._stopping:
    timeout = 0          # items pending → don't block in select
elif self._scheduled:
    timeout = min(max(0, when - self.time()), MAXIMUM_SELECT_TIMEOUT)
else:
    timeout = None       # block indefinitely in select
```

If `_ready` is empty when `_run_once` starts, `select()` blocks until I/O or a timer fires. Items added to `_ready` by another thread via `call_soon` (not threadsafe) will NOT wake the selector.

## `call_soon` vs `call_soon_threadsafe`

| | `call_soon` | `call_soon_threadsafe` |
|---|---|---|
| Appends to `_ready` | Yes | Yes |
| Wakes selector (`_write_to_self`) | **No** | **Yes** |
| Thread-safe | No (GIL protects in practice) | Yes |
| Used by | `Task.__init__`, `Future._schedule_callbacks` | Cross-thread wake-ups |

**Source:** [`Lib/asyncio/base_events.py:call_soon`](https://github.com/python/cpython/blob/v3.11.14/Lib/asyncio/base_events.py#L761), [`call_soon_threadsafe`](https://github.com/python/cpython/blob/v3.11.14/Lib/asyncio/base_events.py#L795)

### The stall pattern

When code on thread A calls `loop.create_task(coro)` (which uses `call_soon`), and the event loop runs on thread B stuck in `select()`:

```
Thread A (GIL): create_task → call_soon → appends to _ready
Thread B:       _run_once → select(timeout=None) → BLOCKED
                            (doesn't know about new _ready items)
```

**Fix:** Call `loop.call_soon_threadsafe(lambda: None)` to poke the self-pipe and wake `select()`.

### Quick test

```bash
uv run python -c "
import asyncio
loop = asyncio.new_event_loop()
print('_ready before:', len(loop._ready))
loop.call_soon(lambda: None)
print('_ready after call_soon:', len(loop._ready))
# call_soon_threadsafe also writes to self-pipe:
loop.call_soon_threadsafe(lambda: None)
print('_ready after threadsafe:', len(loop._ready))
loop.close()
"
```

## `asyncio.Future` Callback Scheduling

`Future.set_result()` does **NOT** fire callbacks synchronously. It schedules them via `call_soon`.

**Source:** [`Modules/_asynciomodule.c:FutureObj_result_set`](https://github.com/python/cpython/blob/v3.11.14/Modules/_asynciomodule.c) and [`Lib/asyncio/futures.py:Future._schedule_callbacks`](https://github.com/python/cpython/blob/v3.11.14/Lib/asyncio/futures.py#L153)

```python
# From CPython futures.py:
def _schedule_callbacks(self):
    for callback in self._callbacks[:]:
        self._loop.call_soon(callback, self)   # NOT immediate!
    self._callbacks[:] = []
```

### Quick test

```bash
uv run python -c "
import asyncio
loop = asyncio.new_event_loop()
fut = loop.create_future()
called = []
fut.add_done_callback(lambda f: called.append('fired'))
fut.set_result(42)
print('called after set_result:', called)        # [] — not fired yet!
print('_ready has callback:', len(loop._ready))   # 1
loop.run_until_complete(asyncio.sleep(0))
print('called after run:', called)                # ['fired']
loop.close()
"
```

**Implication:** If you call `set_result()` on one thread and expect the callback to fire before the event loop runs `_run_once`, it won't. The callback sits in `_ready`.

## `Task.__init__` and `__step` Scheduling

`_asyncio.Task.__init__` (C extension) calls `loop.call_soon(self.__step)`.

**Source:** [`Modules/_asynciomodule.c:task_call_step_soon`](https://github.com/python/cpython/blob/v3.11.14/Modules/_asynciomodule.c)

```
Task.__init__(coro, loop=loop)
  └→ loop.call_soon(self.__step)    # appends Handle to _ready
      └→ __step runs in next _run_once:
          _enter_task(loop, self)
          try:
              result = coro.send(None)
          except StopIteration:
              self.set_result(exc.value)
          else:
              result.add_done_callback(self.__wakeup)
          finally:
              _leave_task(loop, self)
```

### Quick test — verify `_ready` grows

```bash
uv run python -c "
import asyncio
loop = asyncio.new_event_loop()
n = len(loop._ready)
async def s(): pass
t = asyncio.Task(s(), loop=loop)
print(f'_ready grew by {len(loop._ready) - n}')   # 1
print(f'handle: {loop._ready[-1]}')                # <Handle TaskStepMethWrapper>
loop._ready[-1].cancel()                            # cancel __step
print(f'cancelled: {loop._ready[-1].cancelled()}')  # True
loop.close()
"
```

## `_enter_task` / `_leave_task`

These C functions set and clear the "current task" for a loop. Only one task can be entered at a time per loop.

**Source:** [`Lib/asyncio/tasks.py:_enter_task`](https://github.com/python/cpython/blob/v3.11.14/Lib/asyncio/tasks.py), [`Modules/_asynciomodule.c`](https://github.com/python/cpython/blob/v3.11.14/Modules/_asynciomodule.c)

```python
asyncio.tasks._enter_task(loop, task)   # sets current_task() → task
asyncio.tasks._leave_task(loop, task)   # sets current_task() → None
```

**Conflict:** If task A is entered and `__step` for task B tries to enter:
```
RuntimeError: Cannot enter into task <B> while another task <A> is being executed
```

### Quick test

```bash
uv run python -c "
import asyncio
loop = asyncio.new_event_loop()
asyncio.events._set_running_loop(loop)
async def s(): pass
t1 = asyncio.Task(s(), loop=loop)
t2 = asyncio.Task(s(), loop=loop)
asyncio.tasks._enter_task(loop, t1)
print('current_task:', asyncio.current_task())
try:
    asyncio.tasks._enter_task(loop, t2)
except RuntimeError as e:
    print(f'conflict: {e}')
asyncio.tasks._leave_task(loop, t1)
asyncio.events._set_running_loop(None)
loop.close()
"
```

## `_set_running_loop` — Thread-Local State

`asyncio.events._set_running_loop(loop)` sets a **thread-local** variable. `asyncio.get_running_loop()` reads it.

**Source:** [`Lib/asyncio/events.py:_set_running_loop`](https://github.com/python/cpython/blob/v3.11.14/Lib/asyncio/events.py)

- `loop.run_forever()` calls `_set_running_loop(self)` at start, `_set_running_loop(None)` at end
- Each OS thread has its own running loop
- `set_event_loop()` is **process-global** (different from `_set_running_loop`)

### Quick test

```bash
uv run python -c "
import asyncio, threading
loop = asyncio.new_event_loop()
asyncio.events._set_running_loop(loop)
print('main thread:', asyncio.get_running_loop())
def check():
    try: asyncio.get_running_loop()
    except RuntimeError as e: print(f'other thread: {e}')
t = threading.Thread(target=check)
t.start(); t.join()
asyncio.events._set_running_loop(None)
loop.close()
"
```

## uvloop Differences

uvloop is a C/Cython event loop using libuv. Key incompatibilities with CPython asyncio internals:

**Source:** [uvloop architecture docs](https://github.com/magicstack/uvloop/blob/master/docs/index.md)

| CPython asyncio | uvloop |
|---|---|
| `loop._ready` is a `collections.deque` | **`_ready` does not exist** |
| `call_soon` appends to `_ready` | `call_soon` uses libuv C-level callbacks |
| `_ready` is inspectable/cancellable | Internal callback queue is opaque |
| `loop._selector` is a Python selector | libuv handles I/O natively in C |

### Quick test — verify `_ready` absence

```bash
uv run python -c "
import uvloop
loop = uvloop.new_event_loop()
print('has _ready:', hasattr(loop, '_ready'))   # False
loop.close()
"
```

### Implication for `Task.__init__`

Cancelling the auto-scheduled `__step` via `loop._ready[-1].cancel()` **does not work on uvloop**. The handle is in libuv's C callback queue, not in `_ready`. Use `getattr(loop, '_ready', None)` to detect this:

```python
ready = getattr(loop, "_ready", None)
n_before = len(ready) if ready is not None else 0
super().__init__(sentinel, loop=loop)
if ready is not None and len(ready) > n_before:
    ready[-1].cancel()
# On uvloop: __step will run. Use an immediately-completing
# sentinel so __step enters/completes/leaves atomically.
```

## sniffio — Async Library Detection

anyio uses [sniffio](https://github.com/python-trio/sniffio) to detect which async library is running.

**Detection order** (from [`sniffio/_impl.py:current_async_library`](https://github.com/python-trio/sniffio/blob/master/sniffio/_impl.py)):

```
1. sniffio.thread_local.name          (thread-local override)
2. sniffio.current_async_library_cvar  (contextvar override)
3. asyncio.current_task() is not None  → "asyncio"
4. raise AsyncLibraryNotFoundError
```

**Key:** sniffio detects asyncio by checking `asyncio.current_task() is not None`. If `_enter_task` was not called (or failed silently), sniffio cannot detect asyncio, and `anyio.create_task_group()` fails.

### Quick test

```bash
uv run python -c "
import asyncio, sniffio
loop = asyncio.new_event_loop()
asyncio.events._set_running_loop(loop)
# Without current_task:
try: print(sniffio.current_async_library())
except Exception as e: print(f'no task: {e}')
# With current_task:
async def s(): pass
t = asyncio.Task(s(), loop=loop)
asyncio.tasks._enter_task(loop, t)
print(f'with task: {sniffio.current_async_library()}')  # asyncio
asyncio.tasks._leave_task(loop, t)
asyncio.events._set_running_loop(None)
loop.close()
"
```

## anyio TaskGroup Internals

**Source:** [`anyio/_backends/_asyncio.py:TaskGroup`](https://github.com/agronholm/anyio/blob/4.11.0/anyio/_backends/_asyncio.py)

### `create_task_group()` flow

```
anyio.create_task_group()
  → get_async_backend()
    → sniffio.current_async_library()   # must detect "asyncio"
    → import anyio._backends._asyncio
  → TaskGroup()
```

### `TaskGroup.__aexit__` wait pattern

```python
while self._tasks:
    self._on_completed_fut = loop.create_future()
    await self._on_completed_fut          # yields asyncio.Future
```

`task_done` callback (fired when a spawned worker completes):
```python
self._tasks.remove(task)
if self._on_completed_fut is not None and not self._tasks:
    self._on_completed_fut.set_result(None)
```

### `CancelScope.__enter__` / `__exit__`

```python
def __enter__(self):
    self._host_task = current_task()           # must be non-None
    self._tasks.add(host_task)
    _task_states[host_task] = TaskState(...)   # WeakKeyDictionary

def __exit__(self, ...):
    if current_task() is not self._host_task:  # MUST match
        raise RuntimeError("Attempted to exit cancel scope in a different task")
```

**Implication:** `current_task()` must return the same object on enter and exit. If `_enter_task` fails silently on resume (e.g. due to a sentinel `__step` conflict), `CancelScope.__exit__` raises.

### Workers use `loop.create_task()` — the stall risk

`tg.start_soon(worker)` calls `loop.create_task(coro)` which uses `call_soon` (not threadsafe). If the event loop thread is in `select()`, the worker's `__step` won't run until the selector wakes. See [the stall pattern](#the-stall-pattern) above.

## Rust Scheduler + asyncio: The Cross-Thread `_ready` Stall

When a Rust scheduler drives Python coroutines on a **different thread** from the asyncio event loop, any `call_soon` triggered during the drive cycle (e.g. `Task.__init__`, `loop.create_task()` from anyio task groups) adds items to `_ready` without waking the selector.

### The deadlock

```
Tokio thread (GIL):
  1. Drive coroutine via coro.send(None)
  2. Coroutine calls loop.create_task() → call_soon → _ready grows
  3. Coroutine suspends on a non-asyncio awaitable (e.g. Rust Future)
  4. Driver returns, releases GIL

Asyncio thread:
  _run_once → select(timeout=None) → BLOCKED forever
  (_ready has items, but select doesn't know)
```

The asyncio-created tasks (step 2) never run because `select()` never returns.

### Why it's intermittent

- **asyncio Future suspension:** When the driver suspends on an `asyncio.Future` and calls `fut.add_done_callback()`, the callback interacts with the asyncio loop internals, which may indirectly poke the selector. These requests succeed.
- **Rust Future suspension:** When the driver suspends on a Rust-side awaitable, nothing interacts with the asyncio loop. The selector stays blocked. These requests deadlock.
- **Rosetta / emulation:** Different CPU architectures change timing. Under Rosetta (ARM→x86 translation), the libuv poll may return more frequently due to signal handling differences, masking the bug.

### When is a poke needed?

A poke (`call_soon_threadsafe(noop)`) is needed **only** when all three conditions hold:

1. The drive cycle added items to `_ready` via `call_soon` (not `call_soon_threadsafe`) that the handler **depends on** for forward progress.
2. The event loop has no other reason to wake (no thread pool completion, no timer, no I/O).
3. The drive result is **not** `Completed` or `Error` (inline completions have no pending asyncio work).

This occurs when the handler creates asyncio tasks (e.g. `anyio.create_task_group()`, `loop.create_task()`) and then awaits their completion. The tasks' `__step` sits in `_ready`; without a wake, `select()` blocks forever.

### When is a poke NOT needed?

- **Inline completion** (`DriveResult::Completed` / `DriveResult::Error`): the coroutine finished synchronously — no pending asyncio work.
- **Sync handlers run via thread pool** (e.g. FastAPI sync endpoints): the thread pool's own `call_soon_threadsafe` already wakes the event loop when the result future resolves. An extra poke is redundant.
- **Handlers that only yield Rust Futures without creating asyncio tasks**: `_SchedulerTask.__init__` adds exactly 1 item to `_ready` (the sentinel `__step`). This is harmless — the sentinel completes immediately and doesn't require a poke for forward progress.

### The performance trap: unconditional poking

An unconditional `call_soon_threadsafe(lambda: None)` after every drive cycle fixes the deadlock but introduces severe overhead:

1. **`py.eval(c"lambda: None")`** on every poke — compiles+evaluates Python on every call (~10-50µs).
2. **Premature event loop wake-up** — for sync handlers, the thread pool's own `call_soon_threadsafe` already wakes the loop. The extra poke causes a useless `_run_once` cycle (processes `__step` + noop) before the real work arrives.
3. **GIL contention** — the poke call extends the `Python::attach` hold by ~50µs per request. Under 50 concurrent connections, that is ~2.5ms of extra serialized GIL time, plus the event loop thread competing for GIL to process the premature wakes.

Benchmarks showed `resp_wait_p50 = 46ms` for trivial sync handlers like `/api/health` — 96% of total latency was waiting for the suspend-resume round-trip, inflated by the extra event loop work. Streaming throughput dropped 46x vs granian.

### The conditional poke strategy

Track `_ready` growth during the drive cycle:

```
CPython:  loop._ready is accessible  → measure len() delta → poke only if delta > 1
uvloop:   loop._ready does not exist → coalesced poke via dedicated tokio task + Notify
Both:     skip poke entirely when DriveResult is Completed or Error
```

**CPython path** (definitive check):

```python
n_before = len(loop._ready)          # snapshot before create_scheduler_task
# ... drive cycle ...
n_after = len(loop._ready)
if n_after > n_before + 1:           # +1 accounts for _SchedulerTask sentinel __step
    loop.call_soon_threadsafe(noop)   # handler created extra asyncio tasks
```

The `+ 1` threshold accounts for `_SchedulerTask.__init__` which always adds exactly one `__step` to `_ready`. A delta > 1 means the handler itself called `loop.create_task()` or similar.

**uvloop path** (no `_ready` introspection):

Since uvloop's callback queue is opaque, signal a dedicated coalesced poke task via `tokio::sync::Notify`. The poke task batches multiple signals into one `call_soon_threadsafe` call, keeping the overhead off the critical request path and out of the GIL-holding `Python::attach` block.

**Implementation details** (see `crates/framework/src/io/`):

- `cached_noop`: `lambda: None` evaluated once at `EventLoop::init`, reused everywhere.
- `ready_deque`: `getattr(loop, "_ready", None)` cached at init — `Some` on CPython, `None` on uvloop.
- `poke_notify`: `Arc<Notify>` shared between `spawn_and_drive` callers and a dedicated tokio poke task.
- `maybe_poke()` in `io/mod.rs`: the conditional logic used by both `spawn_and_drive` and the drain task.

| Scenario | Unconditional poke | Conditional poke |
|---|---|---|
| Inline completion (`yield_once`) | Poke (wasted) | No poke |
| Sync handler, CPython | Poke (wasted) | No poke (`_ready` delta = 1) |
| Sync handler, uvloop | Poke (wasted) | Coalesced async poke (minimal overhead) |
| TaskGroup handler, CPython | Poke (correct) | Poke (correct, `_ready` delta > 1) |
| TaskGroup handler, uvloop | Poke (correct) | Coalesced poke (correct) |
| Streaming | Poke (per-request overhead) | Conditional (poke only if asyncio tasks created) |

### Diagnostic pattern

Add trace logging to the driver to capture the **yield type** on suspension:

| Yield type | Selector wakes? | Risk |
|---|---|---|
| `asyncio.Future` | Usually yes (via `add_done_callback`) | Low |
| Rust Future / custom awaitable | **No** | **Deadlock** |
| `yield None` (budget exhaustion) | N/A (re-enqueued immediately) | None |

If a request hangs with `steps=0, yield_future=1` and no subsequent traces, the asyncio loop is stuck in `select()` with unprocessed `_ready` items.

## Quick-Test Cheatsheet

All tests use `uv run python -c "..."`. Copy-paste ready.

| What to test | Command |
|---|---|
| `_ready` grows after `call_soon` | `loop.call_soon(f); print(len(loop._ready))` |
| `set_result` is async | `fut.set_result(1); print(called)  # []` |
| uvloop has no `_ready` | `print(hasattr(uvloop.new_event_loop(), '_ready'))` |
| sniffio needs `current_task` | `_enter_task(loop, t); print(sniffio.current_async_library())` |
| `_set_running_loop` is thread-local | See test in section above |
| `_enter_task` conflict | `_enter_task(loop, t1); _enter_task(loop, t2)  # raises` |
| Handle cancel works | `loop._ready[-1].cancel(); print(h.cancelled())` |

## Key Source Files

| File | What |
|---|---|
| [`Lib/asyncio/base_events.py`](https://github.com/python/cpython/blob/v3.11.14/Lib/asyncio/base_events.py) | `_run_once`, `call_soon`, `call_soon_threadsafe`, `create_task` |
| [`Lib/asyncio/futures.py`](https://github.com/python/cpython/blob/v3.11.14/Lib/asyncio/futures.py) | Pure-Python Future (C version in `_asyncio`) |
| [`Lib/asyncio/tasks.py`](https://github.com/python/cpython/blob/v3.11.14/Lib/asyncio/tasks.py) | `_enter_task`, `_leave_task`, `current_task` |
| [`Modules/_asynciomodule.c`](https://github.com/python/cpython/blob/v3.11.14/Modules/_asynciomodule.c) | C Task/Future (production code path) |
| [`Lib/asyncio/events.py`](https://github.com/python/cpython/blob/v3.11.14/Lib/asyncio/events.py) | `_set_running_loop`, `get_running_loop` |
| [`sniffio/_impl.py`](https://github.com/python-trio/sniffio/blob/master/sniffio/_impl.py) | `current_async_library` detection |
| [`anyio/_backends/_asyncio.py`](https://github.com/agronholm/anyio/blob/4.11.0/anyio/_backends/_asyncio.py) | TaskGroup, CancelScope, `_task_states` |
