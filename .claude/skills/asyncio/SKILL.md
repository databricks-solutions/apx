---
name: asyncio
description: Use when debugging event loop hangs, task scheduling issues, call_soon vs call_soon_threadsafe confusion, Future callback timing, _enter_task/_leave_task conflicts, GIL contention patterns, per-step vs per-drive task context, uvloop compatibility problems, sniffio/anyio backend detection failures, streaming backpressure deadlocks, or native runtime context issues on the asyncio thread. Also use when verifying asyncio assumptions via quick Python one-liners.
---

# asyncio Internals Reference

CPython 3.11 baseline. Version-specific differences noted for 3.12+ (eager task factory, `eager_start`) and 3.13+ (free-threaded, per-thread task state).

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

### Synchronous vs deferred callback dispatch

Custom Future implementations (e.g., PyO3 `#[pyclass]` with `set_result`) can fire callbacks **synchronously** — under a lock, take all registered callbacks, release lock, fire them. This is faster (0 cycles to wake vs 1-2 for asyncio.Future) but callbacks must be GIL-safe and must not schedule asyncio work that depends on running before the next drive cycle.

| | `asyncio.Future` | Custom synchronous Future |
|---|---|---|
| Callback dispatch | Deferred via `call_soon` | Immediate (under GIL) |
| Cycles to wake | 1–2 `_run_once` cycles | 0 (instant) |
| Thread requirement | Reactor must run `_run_once` | Any (GIL sufficient) |
| Selector wake needed | Only if reactor in `select()` | No |
| Callback safety | Runs during `_run_once` (normal Python) | Must be GIL-safe, must not re-enter driver |

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

### Python 3.12+: `eager_start=True` and `_swap_current_task`

On 3.12+, `Task.__init__` accepts `eager_start=True`:

```python
# CPython 3.12, tasks.py:
if eager_start and self._loop.is_running():
    self.__eager_start()       # runs coro inline, NO call_soon
else:
    self._loop.call_soon(self.__step, ...)  # queues __step to _ready
```

`__eager_start` uses `_swap_current_task` (NOT `_enter_task`). `_swap_current_task` does **not** check for conflicts — it atomically swaps the current task and returns the previous one. This means eager start can run while another task is "entered" without raising `RuntimeError`.

For instantly-completing coroutines (like a sentinel `async def sentinel(): pass`), `eager_start=True` runs the entire lifecycle during `__init__`. **No `__step` callback ever reaches `_ready`.** This eliminates the dominant source of I1 collisions when using Task subclasses as sentinels.

### 3.12+ C struct: `Task.__init__` MUST be called

The C `TaskObj` struct in `_asynciomodule.c` has fields (`task_context`, `task_name`, `task_num_cancels_requested`) that are only initialized by `Task.__init__`. Skipping `__init__` (e.g., a singleton task reused across requests) leaves these fields uninitialized → **segfault** on any access. 

**Rule:** Always call `super().__init__()` on Task subclasses. Use `eager_start=True` with an instantly-completing coroutine if you want to minimize `_ready` pollution.

### Quick test — `eager_start` on 3.12+

```bash
uv run python -c "
import sys, asyncio
if sys.version_info < (3, 12):
    print('eager_start requires 3.12+'); exit()
loop = asyncio.new_event_loop()
asyncio.events._set_running_loop(loop)
n = len(loop._ready)
async def s(): pass
t = asyncio.Task(s(), loop=loop, eager_start=True)
print(f'_ready grew by {len(loop._ready) - n}')  # 0 — completed inline!
print(f'task done: {t.done()}')                    # True
asyncio.events._set_running_loop(None)
loop.close()
"
```

### Quick test — verify `_ready` grows and pop works (3.11, no eager_start)

```bash
uv run python -c "
import asyncio
loop = asyncio.new_event_loop()
n = len(loop._ready)
async def s(): pass
t = asyncio.Task(s(), loop=loop)
print(f'_ready grew by {len(loop._ready) - n}')   # 1
print(f'handle: {loop._ready[-1]}')                # <Handle TaskStepMethWrapper>
# pop() physically removes; cancel() only sets _cancelled flag
loop._ready.pop()
print(f'_ready after pop: {len(loop._ready) - n}')  # 0
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

### Anti-pattern A5: `_enter_task` held across GIL release

Holding `_enter_task` while executing Python bytecode that may release the GIL is the root cause of cross-thread I1 collisions. CPython's GIL switch interval (default 5ms, `sys.getswitchinterval()`) triggers `eval_breaker` checks periodically during `PyIter_Send`. When the GIL switches to the asyncio thread, any `__step` callback in `_run_once` will call `_enter_task` and collide with the task still "entered" on the other thread.

```
Thread A (GIL, running coro.send()):
  _enter_task(loop, task_A)          ← current = task_A
  PyIter_Send → Python bytecode...
    → eval_breaker fires → GIL released

Thread B (asyncio, acquires GIL):
  _run_once → _ready.popleft():
    task_B.__step → _enter_task(loop, task_B)
    → RuntimeError: task_A is being executed!
```

### Dominant collision source: sentinel `__step`

The collision window from per-step `_enter_task` (~1us) is astronomically unlikely to hit. The **real** A5 problem is the **sentinel `__step`** callback from `_SchedulerTask.__init__`. Each per-request `_SchedulerTask` calls `Task.__init__(_sentinel(), loop=loop)`, which schedules a `__step` callback. Under 50 connections, ~50 sentinel `__step` callbacks pile up in `_run_once`, each calling `_enter_task` — making collisions near-certain.

**Fix: eliminate sentinel `__step` from `_run_once`.**

- **Python 3.11:** Per-request `_SchedulerTask` with `ready.pop()` immediately after `super().__init__(_sentinel())`. This physically removes the sentinel `__step` handle from `_ready`. `Handle.cancel()` is **insufficient** under high concurrency — cancelled handles set `_cancelled=True` but remain in the deque; under load they still cause collisions (confirmed: `cancel()` reduced 60K collisions to 220, `pop()` reduced to 13). Guard with `getattr(loop, "_ready", None)` for uvloop compatibility. See [`src/apx/_task.py`](src/apx/_task.py).
- **Python 3.12+:** Per-request `_SchedulerTask` with `eager_start=True`. The sentinel completes inline during `__init__` (via `_swap_current_task`, not `_enter_task`). No `__step` callback reaches `_ready`.

With no sentinel `__step` in `_run_once`, per-step `_enter_task` on the tokio thread has near-zero collision targets for the initial drive. For continuations (drain task resumptions), driving on the asyncio thread eliminates the A5 risk entirely — `_run_once` processes callbacks sequentially.

### Per-step `_enter_task` granularity

Wrap `_enter_task`/`_leave_task` around each individual `coro.send()` + result classification, not the entire drive loop. The bracket must cover **all code that can execute Python bytecode** (see [PyObject_GetAttr executes Python](#pyobjectgetattr-can-execute-python-bytecode) below), leaving only pure-native budget checks outside.

```
Per-step pattern (safe):                     Per-drive pattern (unsafe):
for step in budget:                          _enter_task(loop, task)
    _enter_task(loop, task)                  for step in budget:
    result = coro.send(None)  # ~1us             result = coro.send(None)
    classify(result)          # may run Python    classify(result)
    _leave_task(loop, task)                  _leave_task(loop, task)
    # budget check — pure native, safe       # A5 window: entire loop (~5ms)
```

**Cost:** 2 Python FFI calls per step (~1us). For a 4-step handler: +4us. For a 1-step handler: +1us. <1% of total request time.

**Between steps:** `asyncio.current_task()` returns `None` during budget checks. This is safe because only native (non-Python) code runs during these phases — no GIL switch trigger, no Python library code observing `current_task()`.

### Where `_enter_task` is safe

| Context | Safe? | Why |
|---|---|---|
| Asyncio thread (during `_run_once`) | **Yes** | Sequential callbacks, no concurrent `_enter_task` |
| Tokio thread (initial drive, no sentinel) | **Yes** | No collision targets in `_run_once` after singleton/eager fix |
| Tokio thread (drain task re-drive) | **No** | Handler-created asyncio tasks may collide |
| Any thread (per-drive, not per-step) | **No** | 5ms window → near-certain collision under load |

**Rule:** Initial drives on the tokio thread are safe (sentinel removal eliminates collision targets). All continuations (drain task) must drive on the asyncio thread via `call_soon_threadsafe(DrainOnLoop)` to stay safe.

### Cross-thread A5 mitigation: `tokio_driving` flag

Even with sentinel `__step` removed, a residual A5 window remains: the tokio thread's per-step `_enter_task` during `spawn_and_drive` can collide with the asyncio thread's per-step `_enter_task` during `drive_on_loop` (inline resume from `ResumeCallback`) or `ReadyQueue::drain`, when the GIL switches during `coro.send()`.

```
Tokio thread:   _enter_task(A) → coro.send() → eval_breaker → GIL released
Asyncio thread: acquires GIL → _run_once → ResumeCallback.__call__
                → drive_on_loop → _enter_task(B) → COLLISION (A still entered)
```

**Mitigation:** An `AtomicBool` flag (`tokio_driving`) on `ReadyQueue` guards the window:

1. `spawn_and_drive` sets the flag **before** `drive_task`, clears it **after**.
2. `ResumeCallback::__call__` checks the flag. If set, enqueues to `ReadyQueue` instead of calling `drive_on_loop` — the task is driven later when the flag is cleared.
3. `ReadyQueue::drain` checks the flag. If set, returns 0 — tasks stay queued.
4. After clearing the flag, `spawn_and_drive` pokes unconditionally if the queue has deferred tasks (`!ready_queue.is_empty()`).

**Ordering:** `Release` on store, `Acquire` on load. The GIL acquire/release provides the memory barrier between threads.

**TOCTOU residual (~0.1% under heavy load):** The flag check and `_enter_task` are not atomic. A `ResumeCallback` may check the flag (`false`), then the tokio thread sets it and enters `drive_task`, then the asyncio thread proceeds to `_enter_task` — colliding. Under 100 concurrent connections / 10s load test, this produced 26 out of ~30K requests (0.087%). The per-step bracket (~1µs window) makes this near-negligible.

| Mitigation layer | Collisions (50 conn, 5s) | Error rate |
|---|---|---|
| None (sentinel `__step` in `_ready`) | ~60,000 | 3.6% |
| Sentinel `pop()` only | 220 | 2.3% |
| Sentinel `pop()` + `tokio_driving` flag | 13 | 0.07% |

**Implementation:** See `crates/framework/src/io/bridge/queue.rs` (`tokio_driving` field) and `crates/framework/src/io/bridge/mod.rs` (`spawn_and_drive`, `ResumeCallback::__call__`).

### `_enter_task` in free-threaded Python (3.13t+)

In free-threaded builds, `_enter_task`/`_leave_task` share `state->current_tasks` with borrowed references — they are **not thread-safe** ([CPython #120974](https://github.com/python/cpython/issues/120974)). Python 3.14 fixes this with per-thread circular doubly-linked lists. Until 3.14+, the GIL must be held continuously from `_enter_task` through `_leave_task`.

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

### Quick test — GIL switch interval

```bash
uv run python -c "
import sys
print(f'default switch interval: {sys.getswitchinterval()}s')
sys.setswitchinterval(0.001)  # 1ms — useful for stress-testing A5
print(f'stress interval: {sys.getswitchinterval()}s')
"
```

## `contextvars` and Drive Cycles

`asyncio.Task.__step` calls `Context.run(self.__step_run_and_handle_result)` on every step, entering the task's context. External drivers must do the same: `PyContext_Enter(ctx)` before each drive, `PyContext_Exit(ctx)` after.

A task that suspends and resumes must re-enter its context because another task (or the reactor) may have entered a different context in between. Copy the context once at task creation (`contextvars.copy_context()`), then re-enter it on each drive.

**Pitfalls:**
- `contextvars.copy_context()` has measurable overhead even for empty contexts ([CPython #136157](https://github.com/python/cpython/issues/136157)) — copy once, re-enter many
- CPython 3.13/3.14 has a bug where context variables can leak across tasks during I/O pauses ([CPython #140947](https://github.com/python/cpython/issues/140947)) — explicit enter/exit per drive protects against this
- In free-threaded builds, `ContextVar` itself is not fully thread-safe ([CPython #121546](https://github.com/python/cpython/issues/121546))

## `_set_running_loop` — Thread-Local State

`asyncio.events._set_running_loop(loop)` sets a **thread-local** variable. `asyncio.get_running_loop()` reads it.

**Source:** [`Lib/asyncio/events.py:_set_running_loop`](https://github.com/python/cpython/blob/v3.11.14/Lib/asyncio/events.py)

- `loop.run_forever()` calls `_set_running_loop(self)` at start, `_set_running_loop(None)` at end
- Each OS thread has its own running loop
- `set_event_loop()` is **process-global** (different from `_set_running_loop`)

**Multi-thread implication:** If a driver thread calls `_set_running_loop(loop)` at init, code executing during drive cycles on that thread sees the loop. The asyncio thread also sees it (via `run_forever()`). But a third thread (e.g., a thread pool executor callback) calling `get_running_loop()` gets `RuntimeError: no running event loop`. Libraries that call `get_running_loop()` from thread pool callbacks will break.

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

## `PyObject_GetAttr` Can Execute Python Bytecode

Attribute access via `PyObject_GetAttr` (used by `getattr()`, `.` notation, and PyO3's `getattr`) can trigger `__getattribute__` or `__getattr__` descriptors on custom types. This means C/Rust code classifying yielded values by probing attributes is **executing Python bytecode** and is subject to GIL switch.

Concrete cases in coroutine drivers:
- Probing `_asyncio_future_blocking` to detect `asyncio.Future` — safe on builtin Future (C slot), but custom Future subclasses may have Python-level descriptors
- Probing `__await__` to detect custom awaitables — always a Python attribute lookup
- Calling `.call_method0("__await__")` — full Python method dispatch

**Rule:** Any code path that calls `PyObject_GetAttr` on user-controlled types must be inside the `_enter_task` bracket. Leaving it outside creates an A5 window identical to `PyIter_Send`.

## Done Callback Thread Identity

Where a done callback fires determines what scheduling APIs are safe to call from it:

| Future type | Callback fires on | `call_soon` safe? | `call_soon_threadsafe` safe? |
|---|---|---|---|
| `asyncio.Future` | asyncio thread (during `_run_once`) | **Yes** (on-loop) | Yes (redundant wake) |
| Custom Future (synchronous dispatch) | Whatever thread called `set_result()` | **Only if on asyncio thread** | Yes |

**Key insight:** `asyncio.Future` done callbacks fire during `_run_once` step 3 (processing `_ready`). Code in these callbacks is on the asyncio thread. `call_soon` (not threadsafe) is correct and ~200ns cheaper than `call_soon_threadsafe` (avoids self-pipe write).

This distinction matters when resuming suspended coroutines from done callbacks. If the callback is known to fire on the asyncio thread (e.g., from `asyncio.Future`), the coroutine can be driven inline — serialized with `_run_once`, no cross-thread scheduling needed. If the callback may fire on another thread, cross-thread mechanisms (`call_soon_threadsafe`, `ReadyQueue`) are required.

### Inline driving from done callbacks

When a done callback fires on the asyncio thread, driving the coroutine directly avoids the cost of enqueuing → waking a drain task → GIL acquisition:

```
Standard path (cross-thread):                Inline path (on asyncio thread):
done_callback fires                           done_callback fires (during _run_once)
  → push to ReadyQueue                          → extract future result
  → Notify drain task                            → drive_task() directly
  → drain acquires GIL                           → (budget exhausted? call_soon to yield)
  → resume_task()
```

When driving inline and the step budget is exhausted, use `call_soon(resume_callback)` to yield back to the event loop. This keeps the task on the asyncio thread but lets `_run_once` process I/O events and other callbacks between drive batches. Do NOT use `call_soon_threadsafe` here — it would write to the self-pipe unnecessarily since you're already on the loop thread.

## GIL Relay Bottleneck

When multiple threads each need the GIL sequentially for a single request, GIL contention serializes them into a latency queue:

```
Request lifecycle requiring 3 GIL hops:
  Thread 1 (tokio): GIL → drive coro → release
  Thread 2 (asyncio): GIL → process _run_once → fire callback → release
  Thread 3 (drain): GIL → resume_task → drive continuation → release

Under N concurrent connections:
  Each hop waits for up to N-1 other threads' GIL holds
  Latency ≈ N × hops × avg_hold_time
```

With 50 concurrent connections and 3 hops per request, `resp_wait_p50` can reach ~17ms even for trivial handlers — 96% of total latency is GIL relay, not actual computation.

**Fix:** Minimize cross-thread GIL hops. Drive continuations on the same thread that receives the done callback (inline driving). This reduces 3 hops to 2 for asyncio Future resumptions, eliminating the drain task from the critical path.

| Pattern | GIL hops/request | Threads involved |
|---|---|---|
| Standard (drain task) | 3 | tokio + asyncio + drain |
| Inline driving (asyncio Future) | 2 | tokio + asyncio |
| Inline completion (no suspension) | 1 | tokio only |

## Native Runtime Context on the asyncio Thread

When a drain callback (e.g., `DrainOnLoop`) runs on the asyncio thread via `call_soon_threadsafe`, it executes inside `_run_once` step 3 — on the asyncio thread, **not** on a native async runtime (Tokio, etc.) worker thread. Any code driven from that callback that needs to interact with the native runtime (spawn tasks, send on channels, resolve backpressure) requires the runtime context to be explicitly available.

### The `enter()` guard pitfall

The natural approach is to call `runtime_handle.enter()` at the top of the callback, which sets the runtime's own thread-local. Downstream code then calls `Runtime::try_current()` to find it:

```
DrainOnLoop.__call__:
    let _guard = handle.enter();      // sets tokio's thread-local
    drive_task(...)
      → Python coro.send()
        → ASGI send() → try_send() → Full → backpressure
          → with_runtime_handle(|h| h.spawn(...))
            → check custom thread-local: None
            → check Runtime::try_current(): ???
```

The problem: if the consumer uses a **two-tier lookup** (custom thread-local first, then `try_current()` as fallback), the `enter()` guard only populates the runtime's own thread-local — it does **not** populate the custom one. If the `try_current()` fallback fails for any reason (e.g., called inside a `thread_local!().with()` closure that interferes with the runtime's own thread-local access, or the guard was dropped prematurely), the lookup returns `None`.

### The reliable fix: explicit thread-local setting

Instead of relying on `enter()` + `try_current()`, explicitly set the custom thread-local at the start of every callback invocation:

```
DrainOnLoop.__call__:
    set_runtime_handle(self.handle.clone());  // sets custom thread-local directly
    drive_task(...)
      → with_runtime_handle(|h| h.spawn(...))
        → check custom thread-local: Some(handle) ✓
```

This is idempotent, costs one thread-local write (~10ns), and eliminates all ambiguity about which thread-local is populated.

### When this matters

| Callback context | Native runtime available? | Action needed |
|---|---|---|
| Tokio worker thread | **Yes** (inherently) | None |
| asyncio thread (via `call_soon_threadsafe`) | **No** | Explicit `set_runtime_handle()` |
| Thread pool thread | **No** | Explicit `set_runtime_handle()` or `handle.enter()` |

**Symptom when missing:** `RuntimeError: no runtime for backpressure send` or `RuntimeError: no tokio runtime found` — the driven Python code hits a path that needs the native runtime, but the asyncio thread has no runtime context.

## Streaming Backpressure and GIL Deadlock

ASGI streaming responses produce body chunks through a bounded channel. When the producer (Python coroutine under GIL) fills the channel faster than the HTTP layer drains it, backpressure engages. The interaction between backpressure resolution and the GIL creates a specific deadlock pattern.

### The deadlock pattern

```
Thread A (asyncio, GIL held via DrainOnLoop):
  1. Drive Python coroutine: coro.send(None)
  2. Coroutine calls ASGI send({type: "http.response.body", more_body: True})
  3. Rust handler: try_send(chunk) → channel full → BACKPRESSURE
  4. Need to spawn native async task to await channel space
  5. Task spawned → task needs to resolve a Python Future when space available
  6. Resolving the Future requires GIL → Python::attach() blocks
  7. DEADLOCK: step 1 holds GIL, step 6 waits for GIL

Thread B (tokio, HTTP layer):
  - Draining the channel by sending bytes to the client
  - Making space, but the spawned task can't signal completion (GIL blocked)
```

### Why it manifests under load

At low concurrency or with small payloads, the HTTP layer drains the channel between drive cycles, so `try_send()` rarely fails. Under load:

- **Drive budget amplification:** A single drive cycle may produce 128+ chunks without yielding. With a channel capacity of 8, backpressure is guaranteed after 8 chunks.
- **CPU starvation:** On constrained CPU (e.g., 0.5 cores), the HTTP layer can't drain fast enough between chunks produced within a single drive's GIL hold.
- **Cascading:** Once one request deadlocks, the GIL is stuck. All other requests waiting for the GIL also stall, creating a total server freeze.

### Channel sizing as a mitigation

Size the channel so that a single drive cycle cannot fill it:

```
channel_capacity > drive_budget
```

If the drive budget is 128 steps, a channel capacity of 256 means a single drive cycle can produce up to 128 chunks without hitting backpressure. The HTTP layer then drains the buffer between drive cycles (when the GIL is released).

| Channel capacity | Drive budget | Backpressure within single drive? | Risk |
|---|---|---|---|
| 8 | 128 | **Yes** (after 8 steps) | **High** — GIL deadlock |
| 128 | 128 | Possible (edge case) | Medium |
| 256 | 128 | **No** | Low |
| 1024 | 128 | **No** | Negligible (more memory) |

**Trade-off:** Larger channels use more memory per stream. For 1000 concurrent streams with 1KB chunks and capacity 256, that's ~250MB of buffered data. Size according to expected concurrency and chunk size.

### The fundamental tension

The deadlock arises from a circular dependency:

1. **Producer** (Python, GIL-holding) wants to write to the channel
2. **Channel** is full, resolution requires native async task
3. **Native task** needs GIL to signal Python Future completion
4. **GIL** is held by the producer → cycle

Breaking any link in this chain fixes the deadlock:

- **Larger channel** → step 2 doesn't occur within a single GIL hold (mitigation, not elimination)
- **GIL-free signaling** → step 3 doesn't need the GIL (requires custom Future with synchronous dispatch; see [Synchronous vs deferred callback dispatch](#synchronous-vs-deferred-callback-dispatch))
- **Yield on backpressure** → step 1 releases GIL before retrying (adds latency, requires coroutine cooperation)
- **Flow control at the ASGI layer** → limit how many chunks the coroutine can produce per drive cycle (application-level, not always possible)

### Diagnostic pattern

```
TRACE asgi::scope: stream chunk BACKPRESSURE (channel full) body_len=N more_body=true
TRACE io::driver: drive: error steps=0 error=RuntimeError: no runtime for backpressure send
```

The `steps=0` indicates the very first step of a continuation drive hit backpressure — the channel was still full from the previous drive. Combined with "no runtime" (missing native runtime context on the asyncio thread, see section above), this produces an immediate drive error.

If the runtime context IS available but the spawned task still deadlocks (no error, just a hang), the symptom is a stream that stops mid-way through. The HTTP client receives partial data, then the connection times out. Server-side, the drive trace shows the last successful chunk followed by silence.

## `asyncio.Future.done()` Fast Path

When handling a yielded `asyncio.Future`, check `fut.done()` before attaching a done callback. If the future is already resolved, extract the result and continue driving — no suspension, no callback overhead, no GIL relay.

```python
# Pseudo-code for the optimization:
if fut.done():
    result = fut.result()       # immediate
    task.set_send_value(result)
    continue                    # re-enter drive loop
else:
    fut.add_done_callback(resume_cb)  # standard suspend path
```

This is especially relevant on Python 3.12+ with eager task factory, where futures from eagerly-started tasks may already be resolved by the time the parent coroutine inspects them.

**Avoid recursion.** If the re-drive after an already-done future yields another already-done future, naive recursive calls grow the stack. Use a loop instead: `handle_drive_result` returns a "continue driving" signal, and the caller loops back into `drive_task`.

## Cancellation Semantics

### Edge-triggered, not level-triggered

`task.cancel()` delivers `CancelledError` **once** at the next `await` point. If the coroutine catches it and `await`s again, cancellation is forgotten:

```python
async def sticky():
    try:
        await asyncio.sleep(10)
    except asyncio.CancelledError:
        print("caught cancel")
        await asyncio.sleep(1)     # succeeds! cancellation is gone
        print("continued normally")
```

This differs from Trio's level-triggered model. `Task.uncancel()` (3.11+) and `Task.cancelling()` manage nesting depth but add their own edge cases.

### Known cancellation bugs

- `asyncio.wait_for()` can raise `CancelledError` instead of `TimeoutError` in race conditions ([CPython #114496](https://github.com/python/cpython/issues/114496))
- `asyncio.timeout(0)` catches and processes a prior **unrelated** cancellation of the enclosing task ([CPython #134471](https://github.com/python/cpython/issues/134471))
- `Task.uncancel()` interacts poorly with `TaskGroup`: the internal cancellation counter can become corrupted ([CPython #95289](https://github.com/python/cpython/issues/95289))
- `TaskGroup._abort()` runs once — tasks created after abort are never cancelled ([CPython #94398](https://github.com/python/cpython/issues/94398))
- Bare `except:` silently swallows `CancelledError` since Python 3.8 changed it from `Exception` to `BaseException` ([CPython #76709](https://github.com/python/cpython/issues/76709))

## Tasks Are Weakly Referenced

`asyncio._all_tasks` is a `WeakSet`. Tasks without external strong references can be garbage-collected while pending:

```python
async def fire_and_forget():
    await asyncio.sleep(10)

asyncio.create_task(fire_and_forget())  # no strong ref saved!
# GC may collect → "Task was destroyed but it is pending!" warning
```

**Implication for drivers:** Between suspension and resumption, the only strong reference to a driven task may be inside the done callback object. If the future holding the callback is GC'd without resolution, the task silently disappears. Ensure at least one strong reference exists for the entire driven lifecycle (e.g., inside the callback struct or a dedicated pending-tasks collection).

### Quick test

```bash
uv run python -c "
import asyncio, gc
loop = asyncio.new_event_loop()
async def never_finishes(): await asyncio.sleep(999)
t = asyncio.Task(never_finishes(), loop=loop)
print(f'all_tasks: {len(asyncio.all_tasks(loop))}')   # 1
del t
gc.collect()
print(f'all_tasks after del: {len(asyncio.all_tasks(loop))}')  # 0 — collected!
loop.close()
"
```

## Signal Handling Requires Main Thread

`loop.add_signal_handler()` only works on the main thread. When the asyncio loop runs on a non-main thread (common in Rust-embedded architectures):

```
asyncio thread: loop.add_signal_handler(SIGINT, handler)
  → RuntimeError: "set_wakeup_fd only works in main thread"
```

Python's `signal` module requires handlers to run on the main thread. `Ctrl-C` sets a CPython flag checked only when control returns to the Python interpreter — not during Rust execution ([PyO3 #3795](https://github.com/PyO3/pyo3/discussions/3795)). uvloop also has hangs after `KeyboardInterrupt` ([uvloop #335](https://github.com/MagicStack/uvloop/issues/335)).

**Workaround:** Handle signals in the native runtime (e.g., `tokio::signal::ctrl_c()`) and broadcast shutdown to the asyncio loop via `call_soon_threadsafe`. Libraries like Uvicorn that call `add_signal_handler` will fail on non-main-thread loops.

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

Removing the auto-scheduled `__step` via `loop._ready` **does not work on uvloop** — `_ready` does not exist. The handle is in libuv's C callback queue. Use `getattr(loop, '_ready', None)` to detect this.

**Use `pop()`, not `cancel()`:** `Handle.cancel()` only sets `_cancelled=True`; the handle remains in the deque and still causes A5 collisions under high concurrency (cancelled handles pile up in `_run_once`, each consuming a `popleft()` + `_cancelled` check cycle, and under extreme load the timing still allows collisions). `deque.pop()` physically removes the handle.

```python
ready = getattr(loop, "_ready", None)
n_before = len(ready) if ready is not None else 0
super().__init__(sentinel, loop=loop)
if not _PY312 and ready is not None and len(ready) > n_before:
    ready.pop()  # physically remove — cancel() is insufficient
# On uvloop: _ready is None, so __step stays in libuv's C queue.
# Use an immediately-completing sentinel so __step enters/completes/
# leaves atomically (~1us, no collision window).
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
- **Handlers that only yield Rust Futures without creating asyncio tasks**: With sentinel `__step` popped (3.11) or `eager_start=True` (3.12+), no sentinel pollution in `_ready`. Nothing new in `_ready` means no poke needed. The drain task's `call_soon_threadsafe(DrainOnLoop)` handles waking the reactor for continuations.

### The performance trap: unconditional poking

An unconditional `call_soon_threadsafe(lambda: None)` after every drive cycle fixes the deadlock but introduces severe overhead:

1. **`py.eval(c"lambda: None")`** on every poke — compiles+evaluates Python on every call (~10-50µs).
2. **Premature event loop wake-up** — for sync handlers, the thread pool's own `call_soon_threadsafe` already wakes the loop. The extra poke causes a useless `_run_once` cycle (processes `__step` + noop) before the real work arrives.
3. **GIL contention** — the poke call extends the `Python::attach` hold by ~50µs per request. Under 50 concurrent connections, that is ~2.5ms of extra serialized GIL time, plus the event loop thread competing for GIL to process the premature wakes.

Benchmarks showed `resp_wait_p50 = 46ms` for trivial sync handlers like `/api/health` — 96% of total latency was waiting for the suspend-resume round-trip, inflated by the extra event loop work. 

### The conditional poke strategy

Track `_ready` growth during the drive cycle:

```
CPython:  loop._ready is accessible  → measure len() delta → poke only if delta > 0
uvloop:   loop._ready does not exist → coalesced poke via dedicated tokio task + Notify
Both:     skip poke entirely when DriveResult is Completed or Error
```

**CPython path** (definitive check):

```python
# n_before MUST be captured AFTER create_scheduler_task, so the
# sentinel __step (3.11, popped) or eager completion (3.12+) is
# already reflected. Only user-code additions matter.
n_before = len(loop._ready)          # snapshot after scheduler task creation
# ... drive cycle ...
n_after = len(loop._ready)
if n_after > n_before:               # handler created asyncio tasks
    loop.call_soon_threadsafe(noop)
```

**Critical:** capture `n_before` **after** `create_scheduler_task` returns, not before. On 3.11 with `ready.pop()`, the sentinel `__step` is already removed; on 3.12+ with `eager_start`, the sentinel completed inline. Either way, `n_before` reflects the clean baseline. Any growth during the drive is genuine user code (`loop.create_task()`, `anyio.create_task_group()`, etc.).

**Bug history:** An earlier implementation used `n_after > n_before + 1` (accounting for the sentinel `__step`). This was incorrect — on 3.12+ with `eager_start`, no `__step` is added, so the `+1` offset meant user-created tasks were never poked. The correct threshold is `n_after > n_before` with `n_before` captured after scheduler task creation.

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
| Sync handler, CPython | Poke (wasted) | No poke (`_ready` delta = 0 after sentinel pop) |
| Sync handler, uvloop | Poke (wasted) | Coalesced async poke (minimal overhead) |
| TaskGroup handler, CPython | Poke (correct) | Poke (correct, `_ready` delta > 0) |
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
| Sentinel `__step` pop works | `Task(s(), loop=l); l._ready.pop(); print(len(l._ready))` |
| GIL switch interval | `sys.getswitchinterval()  # default 0.005` |
| `eager_start` (3.12+) | `Task(s(), loop=l, eager_start=True); print(len(l._ready))  # 0` |
| `fut.done()` before suspend | `fut.set_result(42); print(fut.done())  # True` |
| Tasks are weakly held | `del task; gc.collect(); print(len(all_tasks(loop)))  # 0` |
| Cancel is edge-triggered | catch CancelledError, then await again — succeeds |
| `handle.enter()` sets tokio TL | `let _g = handle.enter(); Handle::try_current().is_ok()` |
| Backpressure on small channel | Fill mpsc(8) from sync code, observe send blocks |

## Key Source Files

| File | What |
|---|---|
| [`Lib/asyncio/base_events.py`](https://github.com/python/cpython/blob/v3.11.14/Lib/asyncio/base_events.py) | `_run_once`, `call_soon`, `call_soon_threadsafe`, `create_task` |
| [`Lib/asyncio/futures.py`](https://github.com/python/cpython/blob/v3.11.14/Lib/asyncio/futures.py) | Pure-Python Future (C version in `_asyncio`) |
| [`Lib/asyncio/tasks.py`](https://github.com/python/cpython/blob/v3.11.14/Lib/asyncio/tasks.py) | `_enter_task`, `_leave_task`, `_swap_current_task` (3.12+), `current_task` |
| [`Modules/_asynciomodule.c`](https://github.com/python/cpython/blob/v3.11.14/Modules/_asynciomodule.c) | C Task/Future, `TaskObj` struct fields, `eager_start` (3.12+) |
| [`Lib/asyncio/events.py`](https://github.com/python/cpython/blob/v3.11.14/Lib/asyncio/events.py) | `_set_running_loop`, `get_running_loop` |
| [`sniffio/_impl.py`](https://github.com/python-trio/sniffio/blob/master/sniffio/_impl.py) | `current_async_library` detection |
| [`anyio/_backends/_asyncio.py`](https://github.com/agronholm/anyio/blob/4.11.0/anyio/_backends/_asyncio.py) | TaskGroup, CancelScope, `_task_states` |

## Invariant Summary

Quick reference for asyncio contracts that external drivers must uphold:

| ID | Invariant | Violation symptom |
|---|---|---|
| I1 | One current task per loop | `RuntimeError: Cannot enter into task X while Y...` |
| I2 | Context entered per drive cycle | `contextvars` return stale values after suspension |
| I3 | `Task.__init__` must be called (3.12+) | Segfault (uninitialized C struct fields) |
| I4 | Cross-thread scheduling needs `call_soon_threadsafe` | Reactor stuck in `select()`, tasks never stepped |
| I5 | `asyncio.Future` callbacks are deferred | Assuming callback fires immediately → missing wake |
| I6 | `_ready` is CPython-specific | `AttributeError` on uvloop, sentinel crash |
| I7 | `Task.__init__` has side effects (`call_soon`) | Sentinel `__step` pollutes `_ready` or C callback queue |
| I8 | Cancellation is edge-triggered | Task survives cancel, cleanup hangs |
| I9 | Tasks are weakly referenced | Silent task disappearance, "destroyed but pending" |
| I10 | Signal handling requires main thread | `RuntimeError: set_wakeup_fd only works in main thread` |
| I11 | `_set_running_loop` is thread-local | `get_running_loop()` returns None on other threads |
| I12 | Native runtime context must be explicit on asyncio thread | `RuntimeError: no runtime for backpressure send` — drain callbacks run on the asyncio thread, which has no native runtime context unless explicitly set |
| I13 | Stream channel capacity must exceed drive budget | Partial streaming response, then hang — producer fills channel within one GIL hold, backpressure resolution deadlocks on GIL |
| I14 | Asyncio thread must not drive inline while tokio thread drives | 500 errors from `anyio.CapacityLimiter` / `CancelScope` — `current_task()` returns wrong task due to cross-thread A5 collision. Guard with `tokio_driving` `AtomicBool` on `ReadyQueue` |
