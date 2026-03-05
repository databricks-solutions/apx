---
name: rust
description: Strict set of rules in terms of codebase development, design patterns, and best practices. Use when the user wants to develop a new feature or refactor existing code.
---

## Principles

Priority: **Correctness > Safety > Readability > Performance**

- **Idiomatic Rust.** Follow standard library conventions. If the stdlib does it one way, do it that way.
- **Leverage the type system.** Encode invariants in types, not runtime checks. Make illegal states unrepresentable.
- **Domain-driven design.** Name everything in the language of the problem, not the implementation.
- **Reuse existing libraries and frameworks** when possible.

## Safety rules

Adapted from [The Power of Ten](https://en.wikipedia.org/wiki/The_Power_of_10:_Rules_for_Developing_Safety-Critical_Code) (Holzmann) for Rust.

1. **Simple control flow.** No nested if-else, no nested loops. Split into single-purpose functions. Max one level of branching per function body.
2. **Short functions.** No function longer than ~60 lines. If it's too long, decompose it.
3. **Fixed loop bounds.** All loops must have a provable upper bound. Prefer iterators (`for x in collection`) over manual `while i < len` with index arithmetic.
4. **Minimal allocation.** Prefer `&str` over `String`, slices over `Vec`, borrows over clones. Allocate only when you must own.
5. **Assertions at boundaries.** Use `debug_assert!` for invariants within functions. Validate inputs at public API boundaries with `Result`/`Option`, not panics.
6. **Smallest possible scope.** Variables, types, and functions should be visible only where needed. Prefer module-private by default; add `pub` only when required.
7. **Handle all return values.** Never discard `Result`. Use `?` propagation or explicit handling. Annotate intentional ignores with `let _ =`.
8. **Macros sparingly.** Prefer generics, traits, and enums over procedural macros. Macros obscure control flow and complicate debugging.
9. **Domain types over raw primitives.** Wrap repetitive low-level operations in a named type instead of scattering the same logic across functions.
```rust
// bad — raw index arithmetic repeated everywhere
let mut i = 0;
while i < bytes.len() {
    if bytes[i] == target { return i; }
    i += 1;
}

// good — domain cursor encapsulates iteration
let mut scanner = Scanner::new(bytes);
while let Some(b) = scanner.peek() {
    if b == target { return scanner.position(); }
    scanner.advance();
}
```
10. **Zero warnings.** Code must compile with `#[deny(warnings)]` cleanly. Run `cargo clippy` and address all lints.
11. **Use `#[expect(lint, reason = "...")]` instead of `#[allow]`.** `#[expect]` warns when the suppression becomes unnecessary, preventing stale silencing. Always include a `reason` string. *(M-LINT-OVERRIDE-EXPECT)*
12. **Panics mean stop the program.** Panics are not exceptions. Never use `panic!`, `unwrap()`, or `unreachable!()` for recoverable errors — use `Result`. `expect()` is acceptable only for proven invariants with a descriptive message. *(M-PANIC-IS-STOP)*
13. **`unsafe` only when required by 3rd-party libraries** (e.g. PyO3 macros). No other reasons to write unsafe code.

## Naming conventions

*Sources: Rust API Guidelines C-CONV, C-GETTER; Microsoft M-CONCISE-NAMES*

- **Conversion prefixes follow ownership semantics:**
  - `as_` — cheap reference-to-reference (no allocation, no copy)
  - `to_` — expensive conversion, may allocate (e.g. `to_string()`)
  - `into_` — consumes self, returns owned value
- **No `get_` prefix on getters.** Use `fn name(&self) -> &str`, not `fn get_name()`.
- **Implement `From<T>`, never `Into<T>`.** The blanket impl gives you `Into` for free.
- **Concise type names.** Avoid hollow suffixes: `Service`, `Manager`, `Factory`, `Handler`, `Processor`. If the name needs a suffix, the type does too much.
- **Named constants over magic literals.** Every literal with domain meaning gets a `const` with a doc comment. No bare numbers, bytes, or strings in logic.
```rust
// bad
if b == b'\\' { i += 2; }

// good
const ESCAPE_BYTE: u8 = b'\\';
if b == ESCAPE_BYTE { scanner.skip_escaped(); }
```

## Error handling

*Sources: Microsoft M-APP-ERROR, M-ERRORS-CANONICAL-STRUCTS*

- **`thiserror` for library crates, `anyhow`/`eyre` for application crates.** Libraries expose structured errors; apps just need context chains.
- **Error messages: lowercase, no trailing punctuation.** Matches `std` convention for composable `.context()` chains.
- **Use `?` propagation everywhere.** Avoid `match` on `Result` when `?` + `.map_err()` suffices.
- **Never `unwrap()` in non-test code.** Use `expect("reason")` only for proven invariants.
- **Canonical error struct pattern:**
```rust
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("invalid token at position {position}")]
    InvalidToken { position: usize, token: char },
    #[error("unexpected end of input")]
    UnexpectedEof,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
```

## Domain-driven design

Adapted from [Domain-Driven Design](https://martinfowler.com/bliki/DomainDrivenDesign.html) (Evans/Fowler) for Rust.

### Ubiquitous language

Name types, functions, and modules in the language of the problem domain, not the implementation.
```rust
// bad — describes implementation mechanics
fn find_end_offset(s: &str) -> Option<usize>
fn check_string(s: &str) -> bool

// good — describes domain concepts
fn PaymentResult::validate(invoice: &Invoice) -> Option<PaymentResult>
fn ClassName::is_tailwind(token: &str) -> bool
```

### Value objects as structs

Domain values without identity are structs. Functions take `&Struct` and return new structs — no mutation through output parameters.
```rust
// bad — caller provides mutable buffer
fn process(input: &Config, out: &mut String)

// good — function returns a value object
fn process(input: &Config) -> ProcessResult
```

### Enums for closed domain rules

When a domain has a fixed set of variants or checks, use an enum — not trait objects, not loose functions.
```rust
// bad — scattered functions, no unifying type
fn is_not_empty(s: &str) -> bool { ... }
fn starts_with_letter(s: &str) -> bool { ... }

// good — enum variants are self-documenting and composable
enum ValidationRule { NonEmpty, StartsWithLetter, ContainsHyphen }
impl ValidationRule {
    fn passes(self, input: &str) -> bool { match self { ... } }
}
const RULES: &[ValidationRule] = &[ValidationRule::NonEmpty, ...];
```

### Traits for open abstractions

Use traits when behavior needs to be extended by future implementations. Start with the trait, then implement concrete types.
```rust
// bad — parallel standalone functions
fn run_git_cmd() -> Output { ... }
fn run_uv_cmd() -> Output { ... }

// good — shared trait, separate implementations
trait ExternalCommand {
    fn execute(&self) -> Result<Output>;
}
impl ExternalCommand for Git { ... }
impl ExternalCommand for Uv { ... }
```

*See also [Service & middleware](#service--middleware) and [Trait object plugin](#trait-object-plugin) in Ecosystem patterns.*

### Modules as bounded contexts

Each Rust module is a bounded context. Types and functions within a module share a domain model; the module boundary is the public API. Keep internal helpers private.

## API design

*Sources: Microsoft M-INIT-BUILDER, M-IMPL-ASREF, M-IMPL-IO, M-AVOID-WRAPPERS; Rust API Guidelines C-COMMON-TRAITS*

- **Builder pattern for complex initialization.** When a type has 4+ optional configuration fields, provide a builder instead of a constructor with many parameters.
- **Accept `impl AsRef<str>` / `impl AsRef<Path>`** over concrete `&str`/`String`/`&Path` in function params when callers may have either type.
- **Accept `impl Read` / `impl Write` for I/O functions.** Decouples logic from concrete I/O sources — enables testing with `Cursor<Vec<u8>>`.
- **Avoid smart pointers in public APIs.** Don't expose `Arc<Mutex<T>>`, `Box<T>`, `Rc<T>` — let callers choose their wrapping strategy.
- **Eagerly implement common traits:** `Debug`, `Clone`, `PartialEq`, `Default` on all public types. *(C-COMMON-TRAITS)*
- **All public types must implement `Debug`.** No exceptions. *(M-PUBLIC-DEBUG)*
- **Avoid unnecessary `Copy`.** Do not derive or implement `Copy` unless the type genuinely benefits from implicit copy semantics. Prefer `Clone` with explicit `.clone()` so copies are visible and intentional.

## Code patterns

### Return values, don't mutate

Functions return domain types instead of writing into `&mut` parameters. This makes data flow explicit and enables composition via `.map()`, `.fold()`, iterators.
```rust
// bad — mutation hides data flow
fn transform(input: &str, out: &mut Vec<String>)

// good — return value makes flow explicit
fn transform(input: &str) -> Vec<TransformResult>
```

### Flat validation with early returns

Split complex validation into a scanning step and a checking step. Each is its own function. No nesting beyond one level.
```rust
// bad — nested ifs, multiple concerns in one block
if b == CLOSE {
    if i + 1 < len && bytes[i + 1] == CLOSE {
        if i == 0 { return None; }
        return Some(i);
    }
    return None;
}

// good — scan finds candidate, validate checks it
fn scan(input: &str) -> Option<Boundary> { ... }     // walks bytes
fn validate(pos: usize, bytes: &[u8]) -> Option<Boundary> { ... }  // checks invariants
```

### Iterator chains over indexed loops

Prefer `.iter()`, `.map()`, `.filter()`, `.collect()` over `for i in 0..len` with manual indexing. Iterator chains are bounds-checked by construction.

### `Cow<'a, str>` for conditional ownership

When a function sometimes borrows and sometimes allocates, return `Cow<'a, str>` instead of always cloning.
```rust
fn normalize(input: &str) -> Cow<'_, str> {
    if input.contains(' ') {
        Cow::Owned(input.replace(' ', "_"))
    } else {
        Cow::Borrowed(input)
    }
}
```

### Pre-allocate when size is known

Use `String::with_capacity()` / `Vec::with_capacity()` when the final size is known or estimable. Avoids repeated reallocations.

### Test-first for bugs

When hitting a bug, write a failing test that reproduces it first. Only then write the fix. Tests document the exact failure mode and prevent regressions.

## Ecosystem patterns

Production Rust relies on patterns popularized by Tokio, Axum, Tower, and Serde. These bridge the gap between the micro-level code patterns above and full application architecture.

### Builder

*Source: [Rust API Guidelines C-BUILDER](https://rust-lang.github.io/api-guidelines/type-safety.html#builders-enable-construction-of-complex-values-c-builder)*

Separate construction from representation. A builder accumulates configuration through method chaining and produces the final value in a terminal `.build()` call that can validate and fail.

> *Expands API design rule: "Builder pattern for complex initialization."*

```rust
pub struct ServerConfig {
    bind_addr: SocketAddr,
    workers: usize,
    tls: Option<TlsConfig>,
}

pub struct ServerConfigBuilder {
    bind_addr: Option<SocketAddr>,
    workers: usize,
    tls: Option<TlsConfig>,
}

impl ServerConfigBuilder {
    pub fn new() -> Self {
        Self { bind_addr: None, workers: 1, tls: None }
    }

    pub fn bind_addr(mut self, addr: SocketAddr) -> Self {
        self.bind_addr = Some(addr);
        self
    }

    pub fn workers(mut self, n: usize) -> Self {
        self.workers = n;
        self
    }

    pub fn tls(mut self, config: TlsConfig) -> Self {
        self.tls = Some(config);
        self
    }

    pub fn build(self) -> Result<ServerConfig, ConfigError> {
        let bind_addr = self.bind_addr.ok_or(ConfigError::MissingBindAddr)?;
        Ok(ServerConfig { bind_addr, workers: self.workers, tls: self.tls })
    }
}
```

**Use when:**
- A type has 4+ optional configuration fields
- Construction requires validation that can fail
- You want to guide callers through configuration step-by-step

**Avoid when:**
- A simple `new()` with 1-3 required fields suffices
- The type is a plain data carrier with no invariants

### Newtype

*Source: [Rust Design Patterns — Newtype](https://rust-unofficial.github.io/patterns/patterns/behavioural/newtype.html)*

Wrap a primitive in a single-field tuple struct to enforce domain invariants at construction time. The inner value is private; access goes through validated constructors and accessor methods.

> *Complements Safety rule #9: "Domain types over raw primitives."*

```rust
// from crates/framework/src/route.rs
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QualName(String);

#[derive(Debug, thiserror::Error)]
pub enum QualNameError {
    #[error("qualified name must not be empty")]
    Empty,
    #[error("qualified name has empty segment: {0}")]
    EmptySegment(String),
    #[error("invalid qualified name segment: {0}")]
    InvalidSegment(String),
}

impl QualName {
    pub fn new(name: impl Into<String>) -> Result<Self, QualNameError> {
        let name = name.into();
        if name.is_empty() {
            return Err(QualNameError::Empty);
        }
        for segment in name.split('.') {
            if segment.is_empty() {
                return Err(QualNameError::EmptySegment(name));
            }
            // ... validate each segment
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
```

**Use when:**
- A raw type (`String`, `u64`, `Vec<u8>`) has domain constraints (non-empty, positive, valid format)
- You need to prevent mixing two semantically different values of the same underlying type
- The type appears in public APIs and you want compile-time safety

**Avoid when:**
- The wrapper adds no invariants and just obscures the inner type
- You only need it in one internal function (use a local `let` binding instead)

### Extension trait

*Source: [Rust API Guidelines C-EXT](https://rust-lang.github.io/api-guidelines/flexibility.html)*

Add methods to a foreign type (one you don't own) by defining a trait and implementing it for that type. Callers `use` the trait to get the new methods.

```rust
// from crates/mcp/src/tools/mod.rs
pub trait ToolResultExt {
    fn from_serializable(value: &impl StructuredObject) -> Self;
    fn from_serializable_error(value: &impl StructuredObject) -> Self;
}

impl ToolResultExt for CallToolResult {
    fn from_serializable(value: &impl StructuredObject) -> Self {
        build_structured_result(value, false)
    }

    fn from_serializable_error(value: &impl StructuredObject) -> Self {
        build_structured_result(value, true)
    }
}

// Usage: CallToolResult::from_serializable(&my_response)
```

**Use when:**
- You need to add domain-specific methods to a type from an external crate
- Multiple call sites would otherwise repeat the same conversion/construction logic
- The methods form a coherent semantic group (name the trait after the capability, e.g. `*Ext`)

**Avoid when:**
- A free function would be equally clear and doesn't benefit from method syntax
- You own the type — just add methods directly

### Type-state

*Source: [Cliffle — Rust Typestate Pattern](https://cliffle.com/blog/rust-typestate/)*

Encode protocol states as zero-sized type parameters. Methods that are only valid in a specific state are only available on that parameterization. State transitions consume the old value and return a new one.

```rust
pub struct Disconnected;
pub struct Connected;

pub struct Connection<S> {
    addr: SocketAddr,
    _state: std::marker::PhantomData<S>,
}

impl Connection<Disconnected> {
    pub fn new(addr: SocketAddr) -> Self {
        Self { addr, _state: PhantomData }
    }

    pub async fn connect(self) -> Result<Connection<Connected>, io::Error> {
        // ... establish connection ...
        Ok(Connection { addr: self.addr, _state: PhantomData })
    }
}

impl Connection<Connected> {
    pub async fn send(&self, data: &[u8]) -> Result<(), io::Error> {
        // only available when connected
        Ok(())
    }
}
```

**Use when:**
- An object has a clear lifecycle with distinct phases (disconnected → connected, unvalidated → validated)
- Calling methods out of order is a logic error you want to catch at compile time
- State transitions are linear and well-defined

**Avoid when:**
- States are dynamic or user-driven (use an enum instead)
- The number of states or transitions is large — type-state combinatorics explode quickly

### Derive macro

*Source: [The Little Book of Rust Macros](https://veykril.github.io/tlborm/)*

Declarative (`macro_rules!`) or procedural macros that auto-derive trait implementations or generate boilerplate. Used sparingly, they eliminate repetitive patterns that generics and traits alone cannot.

> *Controlled exception to Safety rule #8: "Macros sparingly." Derive macros are acceptable when the pattern is mechanical, repeated across many types, and error-prone to write by hand.*

```rust
// from crates/mcp/src/tools/mod.rs
macro_rules! tool_response {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $( $(#[$field_meta:meta])* $field_vis:vis $field:ident : $ty:ty ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, serde::Serialize)]
        $vis struct $name {
            $( $(#[$field_meta])* $field_vis $field : $ty, )*
        }
        impl $crate::tools::StructuredObject for $name {}
    };
}

// Usage — every tool response struct gets Serialize + StructuredObject:
// tool_response! { pub struct MyToolOutput { pub result: String } }
```

**Use when:**
- The same trait implementation is mechanically identical across 5+ types
- Forgetting the impl is a common source of bugs (like the `StructuredObject` marker above)
- The macro body is short and readable

**Avoid when:**
- Generics, blanket impls, or `#[derive(...)]` from serde/thiserror already handle it
- The macro hides non-trivial control flow or business logic
- Only 1-2 types need the pattern (just write the impls by hand)

### Service & middleware

*Source: [Tower — Service trait](https://docs.rs/tower/latest/tower/trait.Service.html)*

A service is a trait with a single async `call` method that transforms a request into a response. Middleware wraps an inner service, adding cross-cutting behavior (logging, auth, timeouts) without modifying business logic. Services compose into layered stacks.

> *Architectural application of DDD "Traits for open abstractions." See also [Trait object plugin](#trait-object-plugin).*

```rust
// from crates/framework/src/bridge/dispatch.rs
// The trait defines the service contract:
pub trait HandlerDispatch: Send + Sync + std::fmt::Debug {
    fn handle(
        &self,
        route: Arc<BoundRoute>,
        app_state: Arc<AppState>,
        request: InboundRequest,
    ) -> Pin<Box<dyn Future<Output = Result<OutboundResponse, AppError>> + Send>>;
}

// A concrete service implements the trait:
pub struct RequestResponseDispatch;

impl HandlerDispatch for RequestResponseDispatch {
    fn handle(
        &self,
        route: Arc<BoundRoute>,
        app_state: Arc<AppState>,
        mut request: InboundRequest,
    ) -> Pin<Box<dyn Future<Output = Result<OutboundResponse, AppError>> + Send>> {
        Box::pin(async move {
            let ctx = extract_context(&mut request, &route, &app_state).await?;
            let result = invoke_handler(&route, &ctx).await?;
            Python::attach(|py| serialize_result(py, &result, &route))
        })
    }
}

// Layered call chain:
// axum handler → transport conversion → HandlerDispatch trait → Python bridge
```

**Use when:**
- You have cross-cutting concerns (auth, logging, metrics, rate limiting) that apply to many handlers
- Multiple dispatch strategies share the same request/response contract
- You want to swap or layer behaviors without modifying core logic

**Avoid when:**
- There's only one handler and no middleware needed — a plain function is simpler
- The "service" would only ever have one implementation (use a concrete type)

### Async trait + future abstraction

*Source: [Rust Reference — async fn in traits](https://blog.rust-lang.org/2023/12/21/async-fn-rpit-in-traits.html)*

Before Rust 1.75, async methods in traits required returning `Pin<Box<dyn Future<...> + Send>>` manually. Since 1.75, `async fn` works directly in traits, but boxed futures remain necessary when you need trait objects (`dyn Trait`).

> *See also [Async & concurrency](#async--concurrency).*

```rust
// Pre-1.75 style (still needed for dyn dispatch):
// from crates/framework/src/bridge/dispatch.rs
pub trait HandlerDispatch: Send + Sync {
    fn handle(
        &self,
        route: Arc<BoundRoute>,
        app_state: Arc<AppState>,
        request: InboundRequest,
    ) -> Pin<Box<dyn Future<Output = Result<OutboundResponse, AppError>> + Send>>;
}

// Post-1.75 style (when dyn dispatch is not needed):
pub trait Processor: Send + Sync {
    async fn process(&self, input: &[u8]) -> Result<Vec<u8>>;
}
```

**Use when:**
- The trait will be used as `dyn Trait` (e.g. stored in a `Vec<Box<dyn Trait>>`) — boxed futures are required
- You need `Send` bounds on the future for multi-threaded runtimes

**Avoid when:**
- The trait is only used with generics (`impl Trait` / `T: Trait`) — use native `async fn` directly
- You can use the `async-trait` crate and its overhead is acceptable

### Trait object plugin

*Source: [Rust Design Patterns — Strategy](https://rust-unofficial.github.io/patterns/patterns/behavioural/strategy.html)*

Store `Arc<dyn Trait>` in a registry to support open-ended extension at runtime. Each plugin implements a shared trait. The registry dispatches by name or type, enabling plugin-style architectures.

> *Architectural application of DDD "Traits for open abstractions."*

```rust
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn init(&self) -> Result<()>;
    fn handle(&self, input: &[u8]) -> Result<Vec<u8>>;
}

pub struct PluginRegistry {
    plugins: Vec<Arc<dyn Plugin>>,
}

impl PluginRegistry {
    pub fn register(&mut self, plugin: Arc<dyn Plugin>) {
        self.plugins.push(plugin);
    }

    pub fn dispatch(&self, name: &str, input: &[u8]) -> Result<Vec<u8>> {
        let plugin = self.plugins.iter()
            .find(|p| p.name() == name)
            .ok_or_else(|| anyhow!("unknown plugin: {name}"))?;
        plugin.handle(input)
    }
}
```

**Use when:**
- The set of implementations is open-ended and grows over time (plugins, tool handlers, codecs)
- Implementations are registered at runtime (e.g. from config or feature flags)
- You need to store heterogeneous implementations in one collection

**Avoid when:**
- The set of variants is closed and known at compile time — use an enum with `match`
- Dynamic dispatch overhead matters in a hot loop — use generics or enum dispatch

### RAII guard

*Source: [Rust Book — Drop trait](https://doc.rust-lang.org/book/ch15-03-drop.html)*

Implement `Drop` on a wrapper type to guarantee cleanup when the value goes out of scope — even on panics or early returns. The guard owns the resource and releases it deterministically.

```rust
// from crates/framework/src/bridge/streaming.rs
impl Drop for AsgiBodyStream {
    fn drop(&mut self) {
        if let Some(task) = self.handler_task.take() {
            task.abort();
        }
    }
}

// The pattern: wrap a resource in a guard that cleans up on drop
pub struct TempDir {
    path: PathBuf,
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
```

**Use when:**
- A resource must be released regardless of control flow (files, tasks, locks, temp dirs)
- Forgetting cleanup is a common source of leaks or bugs
- The resource has a clear owner with a well-defined lifetime

**Avoid when:**
- The cleanup is optional or caller-dependent — provide an explicit `.close()` method instead
- Shared ownership (`Arc`) makes the drop point unpredictable — consider explicit shutdown signals

## Documentation

*Sources: Microsoft M-CANONICAL-DOCS, M-FIRST-DOC-SENTENCE, M-MODULE-DOCS, M-DOC-INLINE*

- **Summary sentence: one line, max 15 words.** First line of `///` is the summary — keep it tight.
```rust
/// Parses a TOML configuration file into a validated config.
```
- **Canonical doc sections** (in order, only when applicable): `# Examples`, `# Errors`, `# Panics`, `# Safety`.
- **Explain parameters in prose,** not parameter tables. Prose reads better and scales to complex interactions.
- **Module-level docs with `//!`** on every public module. Explain what the module provides and when to use it.
- **Use `#[doc(inline)]` on re-exports** to surface docs at the re-export site, not buried in submodules.

## Async & concurrency

*Sources: Microsoft M-YIELD-POINTS; Cloudflare clippy::await_holding_lock*

- **Never block the async runtime.** No `std::thread::sleep()`, no blocking I/O, no heavy CPU work in async context. Use `tokio::task::spawn_blocking()` instead.
- **Yield in long CPU-bound async loops.** Insert `tokio::task::yield_now().await` periodically (every ~10-100us of CPU work) to avoid starving other tasks. *(M-YIELD-POINTS)*
- **No locks across `.await` points.** A `MutexGuard` held across an `.await` can deadlock or block the runtime. Scope the lock, copy data out, then await.
```rust
// bad — guard held across await
let data = lock.lock().expect("lock poisoned");
send(data.clone()).await;

// good — lock released before await
let data = { lock.lock().expect("lock poisoned").clone() };
send(data).await;
```
- **Use `tokio::select!` for concurrent operations** with cancellation semantics.
- **Prefer `std::sync::OnceLock`** over `lazy_static!` or `once_cell::sync::Lazy` for one-time initialization.
- See also [Async trait + future abstraction](#async-trait--future-abstraction) in Ecosystem patterns.

## Lints & static analysis

*Sources: Microsoft M-STATIC-VERIFICATION; Cloudflare foundations*

- **Recommended `Cargo.toml` lint config:**
```toml
[lints.rust]
unreachable_pub = "warn"

[lints.clippy]
unwrap_used = "warn"
clone_on_ref_ptr = "warn"
await_holding_lock = "deny"
large_futures = "warn"
```
- **`#[expect]` over `#[allow]`, always with `reason`.** `#[expect]` warns when the suppression is no longer needed — prevents lint rot.
- **Run `cargo clippy -- -D warnings`** as the zero-warnings gate. No code merges with clippy warnings.

## Testing

- **Descriptive test names.** Use `test_<unit>_<scenario>_<expected>` format: `test_parse_empty_input_returns_error`.
- **Use `tempfile` for filesystem tests.** Never write to hardcoded paths or the working directory.
- **Assert `Send`/`Sync` at compile time** for types that cross thread boundaries:
```rust
fn _assert_send<T: Send>() {}
fn _assert_sync<T: Sync>() {}

#[test]
fn connection_pool_is_send_sync() {
    _assert_send::<ConnectionPool>();
    _assert_sync::<ConnectionPool>();
}
```
- **Test error `Display` output** and verify `.source()` chains — error messages are part of the public API.
- **Use `#[should_panic(expected = "...")]`** for panic tests — always include the `expected` substring.
