---
name: rust
description: Strict set of rules in terms of codebase development, design patterns, and best practices. Use when the user wants to develop a new feature or refactor existing code.
---

## Principles

- Safety
- Readability
- Domain-driven design
- Reuse existing libraries and frameworks when possible

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

## Code patterns

### Return values, don't mutate

Functions return domain types instead of writing into `&mut` parameters. This makes data flow explicit and enables composition via `.map()`, `.fold()`, iterators.
```rust
// bad — mutation hides data flow
fn transform(input: &str, out: &mut Vec<String>)

// good — return value makes flow explicit
fn transform(input: &str) -> Vec<TransformResult>
```

### Named constants over magic literals

Every literal with domain meaning gets a `const` with a doc comment. No bare numbers, bytes, or strings in logic.
```rust
// bad
if b == b'\\' { i += 2; }

// good
const ESCAPE_BYTE: u8 = b'\\';
if b == ESCAPE_BYTE { scanner.skip_escaped(); }
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

### Test-first for bugs

When hitting a bug, write a failing test that reproduces it first. Only then write the fix. Tests document the exact failure mode and prevent regressions.


### Lints and allows

Do not introduce new `allow` annotations unless absolutely necessary. If a lint or rule is disabled, add a comment explaining why.