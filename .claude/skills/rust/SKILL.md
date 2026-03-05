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
let data = lock.lock().unwrap();
send(data.clone()).await;

// good — lock released before await
let data = { lock.lock().unwrap().clone() };
send(data).await;
```
- **Use `tokio::select!` for concurrent operations** with cancellation semantics.
- **Prefer `std::sync::OnceLock`** over `lazy_static!` or `once_cell::sync::Lazy` for one-time initialization.

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
