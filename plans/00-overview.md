# wt-core Review → Implementation Plans

Plans derived from the codebase review conducted 2026-02-13.
Ordered by priority (impact × risk).

| # | Plan | Type | Priority | Risk |
|---|------|------|----------|------|
| 01 | [Extract command layer from main.rs](01-extract-command-layer.md) | refactor | medium | low |
| 02 | [Fix shell binding fragility and bugs](02-fix-shell-binding-bugs.md) | fix | **high** | medium |
| 03 | [Improve git error classification](03-improve-git-error-classification.md) | fix | **high** | low |
| 04 | [RepoRoot and BranchName type ergonomics](04-reporoot-ergonomics.md) | refactor | medium | low |
| 05 | [Clean up CdPath format leaking](05-cdpath-format-leaking.md) | refactor | low | low |

## Recommended execution order

1. **03 — Error classification** (standalone fix, no dependencies, improves correctness)
2. **02 — Shell binding bugs** (user-facing fix, adds `--print-paths` to core)
3. **01 — Extract command layer** (structural refactor, makes future changes easier)
4. **04 — Type ergonomics** (builds on 01's cleaner structure)
5. **05 — CdPath format** (small cleanup, best done after 01)

Plans 01, 04, and 05 can be combined into a single refactor PR if preferred.
Plans 03 and 02 are fully independent and can land in any order.
