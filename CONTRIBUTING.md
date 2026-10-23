# Contributing

Thanks for wanting to help. Bug reports, docs fixes and pull requests are
all welcome.

## Before you start

For anything bigger than a small fix, open an issue first to talk it
through, so your time doesn't go into something that won't fit.
[docs/DESIGN.md](docs/DESIGN.md) explains the goals and the decisions
behind them, and it's the best place to check whether an idea fits.

## Building and testing

```sh
cargo build --workspace
cargo test --workspace                          # no GPU needed
WORLDS_GPU_TESTS=1 cargo test -p worlds-editor  # editor rendering tests, if you have a GPU
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
godot/tests/run.sh                              # the Godot addon (Godot 4.3+ on PATH, or $GODOT)
```

CI runs all of these except the GPU tests on every pull request, and a pull
request can only be merged once they pass.

## Pull requests

- Keep each one focused on one change, with tests for new behaviour and bug
  fixes.
- The docs are part of the contract. `docs/format.md`, `docs/commands.md`,
  `docs/api.md` and `docs/mcp.md` are checked by the tests (their examples
  are run), so update them together with the code. `crates/worlds-mcp/src/catalog.rs`
  is generated from `docs/mcp.md`:
  `WORLDS_WRITE_CATALOG=1 cargo test -p worlds-mcp --test docs catalog`.
- File-format changes must keep existing projects loading. Say in the PR
  how old files are handled.
- For editor UI changes, include a screenshot. `worlds --screenshot` can
  render one headless (see `crates/worlds-editor/README.md`).

By contributing you agree that your contribution is licensed under the
project's licence, GPL-3.0 ([LICENSE](LICENSE)).
