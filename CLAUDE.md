# MCPortal

Rust MCP gateway (single crate). Sources of truth, not duplicated here:
- `ARCHITECTURE.md`: how the code works **now** (modules, lifecycle, metrics).
- `docs/adr/`: **why** it works that way (one Architecture Decision Record per structural decision).
- `CONTRIBUTING.md`: conventions and PR checklist.

## Commands
```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo bench --bench overhead                  # if gateway.rs, upstream.rs, limits.rs or state.rs changed
cargo test --test guide_drift -- --ignored    # real network; only if registry.yaml or guide-ssp.md changed
cargo run -- --config registry.yaml           # local gateway on :8000/mcp, metrics on :9090
```

## Agent workflow
For a feature, a new config key, a change on the call path or a non-trivial refactor, the
main session orchestrates and does not code itself:

1. **architect** → writes `.claude/plans/<feature>.md` and, for a structural decision, an ADR
   `docs/adr/NNNN-….md` (status Proposed). Show me both and wait for my approval.
2. **rust-dev** → implements the tasks, updates `ARCHITECTURE.md` in the same branch and sets
   the ADR to Accepted. Split into several sequential rust-dev runs if the
   plan has independent tasks; never two in parallel on the same files.
3. **reviewer** → on CHANGES REQUIRED, send BLOCKING / SHOULD FIX items back to rust-dev, then re-review.
4. **security-reviewer** → run it when the change touches HTTP handling, tool arguments,
   upstream relaying, catalog/guide content, logs/metrics, dependencies, Docker, k8s or CI.
   On BLOCKED, same loop.
5. At most 2 fix loops; beyond that, stop and ask me.

Skip the squad for small fixes (typo, docs, one-line change, `website/`): do them directly,
then run the checks above.

Branches `feat/…`, `fix/…`, `docs/…`; Conventional Commits; PR against `main` with the
CONTRIBUTING checklist.
