# Plan: Extract command layer from main.rs

## 1. Summary

`main.rs` is 263 lines and contains all five `cmd_*` functions plus their output formatting logic. Each command has a near-identical `match fmt` block for Human/Json/CdPath output. The `cmd_doctor` function defines `JsonDoctorResponse` and `JsonDiag` types inline. This should be split into a dedicated `commands.rs` module, with doctor response types moved to `output.rs`.

## 2. Branching Strategy

`refactor/extract-command-layer`

## 3. Investigation

None required. The boundaries are already clear — the five `cmd_*` functions, `fmt_flag`, and `resolve_repo` are the extraction targets.

## 4. Implementation Steps

1. **Move doctor response types to `output.rs`.**
   - Move `JsonDoctorResponse` and `JsonDiag` from the inline definition in `cmd_doctor()` to `output.rs` as public types.
   - Add `impl JsonDoctorResponse` with a `from_diagnostics()` constructor, mirroring `JsonListResponse::from_worktrees()`.

2. **Create `src/commands.rs` module.**
   - Move `resolve_repo()`, `fmt_flag()`, and all five `cmd_*` functions from `main.rs` into `commands.rs`.
   - Make `resolve_repo` and `fmt_flag` private to the module; expose `pub fn run(cli: Cli) -> error::Result<()>` as the single entry point.

3. **Add a `print_json` helper to reduce boilerplate.**
   - Add a private helper in `commands.rs`:
     ```rust
     fn print_json(value: &impl serde::Serialize) -> error::Result<()> {
         println!("{}", serde_json::to_string_pretty(value)
             .map_err(|e| AppError::git(format!("json error: {e}")))?);
         Ok(())
     }
     ```
   - Replace the 5 duplicated `serde_json::to_string_pretty` + `map_err` blocks with calls to `print_json`.

4. **Slim down `main.rs`.**
   - `main.rs` should contain only `mod` declarations, `main()`, and the `Cli::parse()` → `commands::run()` call. Target: ~20 lines.

5. **Verify all pre-commit gates pass.**
   - `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test`, `ast-grep scan src/`.

## 5. Testing Approach

No new tests needed. All 38 existing tests exercise the command layer through the binary — they validate behavior, not internal module structure. All must remain green.

## 6. Review and Merge

- Confirm `main.rs` is reduced to entry-point-only.
- Confirm no public API changes (binary behavior is identical).
- Squash merge with message: `refactor: extract command layer from main.rs`.
