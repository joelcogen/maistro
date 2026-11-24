# Repository checks

For Rust changes, run the formatting check, linter, and tests before committing.

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

- For installer changes, run `python3 -m unittest discover -s tests -p 'test_install.py'`.
- For status UI changes, run `node --test tests/status_ui.test.cjs`.
- For changes that affect the release build, run `cargo build --locked --release`.
- For documentation changes, check the edited instructions against the
  implementation and verify any new or changed links.

Update the README and example configuration when changing user-facing settings.
