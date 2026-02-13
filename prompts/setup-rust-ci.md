# Set up CI, release, and supply chain infrastructure for a Rust CLI project

## Context

I have a Rust CLI project hosted on GitHub. I need full CI/CD infrastructure. Read the entire repository first to understand the project structure, binary name, and any shell wrappers or bindings before proceeding.

## Requirements

### 1. GitHub Actions secrets (manual prerequisite)

Before running the workflows, I need these secrets added to the repo:
- `CARGO_REGISTRY_TOKEN` — from https://crates.io/settings/tokens
- `CODECOV_TOKEN` — from https://app.codecov.io

### 2. CI workflow (`.github/workflows/ci.yml`)

Trigger on push to `main` and on pull requests. Create these independent jobs:

**`check`** — matrix across `[ubuntu-latest, macos-latest]`:
- `actions/checkout@v6`
- `dtolnay/rust-toolchain@stable` with components `rustfmt, clippy`
- `actions/cache@v5` for `~/.cargo/registry`, `~/.cargo/git`, `target` keyed on `Cargo.lock`
- `cargo fmt --check`
- `cargo clippy -- -D warnings`
- `cargo test`

**`audit`** — ubuntu-latest only:
- `actions/checkout@v6`
- `EmbarkStudios/cargo-deny-action@v2`

**`coverage`** — ubuntu-latest only:
- Install Rust with `llvm-tools` component
- `cargo install cargo-llvm-cov`
- Cache cargo (use a separate cache key like `${{ runner.os }}-cargo-cov-...`)
- `cargo llvm-cov --lcov --output-path target/coverage.lcov`
- Upload via `codecov/codecov-action@v5` with `token: ${{ secrets.CODECOV_TOKEN }}` and `fail_ci_if_error: true`

**`ast-grep`** (only if `sgconfig.yml` exists) — ubuntu-latest only:
- `npm install -g @ast-grep/cli`
- `ast-grep scan src/`

**Shell binding tests** (only if `bindings/` directory exists):
- Build the binary with `cargo build --release`
- Add `target/release` to `$GITHUB_PATH`
- Install shells that aren't pre-installed on ubuntu (`sudo apt-get install -y zsh fish`; use `hustcer/setup-nu@v3` for Nu)
- Run one test script per shell: `bash tests/bindings/bash_test.bash`, `zsh tests/bindings/zsh_test.zsh`, `fish tests/bindings/fish_test.fish`, `nu tests/bindings/nu_test.nu`
- Each test script should: create a temp dir with `trap` cleanup (or equivalent), init a git repo with one empty commit, source the binding, exercise the core commands (add/go/list/remove or equivalent), assert cwd changes and output, and exit non-zero on any assertion failure
- For the Nu test, `cd /tmp` before `^rm -rf $work` cleanup to avoid the `$env.PWD points to a non-existent directory` error that occurs when the cwd is deleted

### 3. Release workflow (`.github/workflows/release.yml`)

Trigger on tag push matching `v*`. Set `permissions: contents: write`.

**`validate`** job:
- Extract version from tag (`${GITHUB_REF#refs/tags/v}`)
- Validate it's semver (`^[0-9]+\.[0-9]+\.[0-9]+$`)
- Verify it matches `Cargo.toml` `version` field — fail with `::error::` if mismatch
- Output `version` and `tag` for downstream jobs

**`ci`** job (needs `validate`):
- Same fmt + clippy + test matrix as the CI workflow (ubuntu + macos)

**`build`** job (needs `validate`, `ci`):
- Matrix across targets:
  - `x86_64-unknown-linux-gnu` on `ubuntu-latest`
  - `x86_64-apple-darwin` on `macos-latest`
  - `aarch64-apple-darwin` on `macos-latest`
  - `x86_64-pc-windows-msvc` on `windows-latest`
- Build with `cargo build --release --target ${{ matrix.target }}`
- Package unix targets as `.tar.gz` (`tar -czf ... -C staging .`)
- Package windows as `.zip` using `7z a` with `shell: bash` and copy `wt-core.exe`
- Upload artifact with both `*.tar.gz` and `*.zip` paths

**`publish`** job (needs `ci`):
- `cargo publish --token "${{ secrets.CARGO_REGISTRY_TOKEN }}"`

**`release`** job (needs `validate`, `build`, `publish`):
- `actions/download-artifact@v7`
- `softprops/action-gh-release@v2` with `generate_release_notes: true`

### 4. cargo-deny config (`deny.toml`)

Create a minimal config:
- `[advisories]` — `ignore = []`
- `[licenses]` — `allow` list with licenses used by the project's dependencies (run `cargo deny check` locally to determine the exact set; common ones: `MIT`, `Apache-2.0`, `Unicode-3.0`)
- `[bans]` — `multiple-versions = "warn"`, `wildcards = "allow"`
- `[sources]` — `unknown-registry = "warn"`, `unknown-git = "warn"`, allow only crates.io

Run `cargo deny check` locally to verify the config passes before committing.

### 5. Dependabot (`.github/dependabot.yml`)

Weekly updates for both `cargo` and `github-actions` ecosystems.

### 6. Cargo.toml hygiene

- Ensure `Cargo.lock` is committed (binary crate)
- Add CI-only files to `exclude` list: `".github/"`, `".ast-grep/"`, `"scripts/"`, `"sgconfig.yml"`, `"deny.toml"`, etc.
- Verify crates.io metadata is complete: `description`, `license`, `repository`, `readme`, `keywords`, `categories`

## Process

1. Read the repo to understand project structure and binary name
2. Create `deny.toml` and validate with `cargo deny check`
3. Create `.github/dependabot.yml`
4. Create shell binding test scripts (if applicable) and validate locally
5. Create/update `.github/workflows/ci.yml`
6. Create/update `.github/workflows/release.yml`
7. Update `Cargo.toml` exclude list
8. Commit, push, and verify all CI jobs pass
9. If any jobs fail, read the logs, fix, and re-push until green

Replace all occurrences of `wt-core` with the actual binary/crate name from the target repo.
