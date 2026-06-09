# Release 0.4.0 checklist

Do not publish crates, push tags, or create a GitHub release without explicit approval from the project lead.

## Version and metadata

- [ ] Confirm `crates/gleam-types/Cargo.toml` has `version = "0.4.0"`.
- [ ] Confirm `crates/beamr/Cargo.toml` has `version = "0.4.0"`.
- [ ] Confirm internal path dependencies include matching crates.io version fallbacks (`beamr -> gleam-types = 0.4.0`, `beamr-cli -> beamr = 0.4.0`).
- [ ] Confirm both published crates declare license, description, repository, and readme metadata.
- [ ] Confirm `cargo metadata --no-deps --format-version 1` contains no stale `0.3.15`, `0.1.0`, or `git+https://github.com/gleam-lang/gleam` publish blockers.

## Publish dry-runs

- [ ] Run `cargo publish --dry-run -p gleam-types`.
- [ ] After `gleam-types` dry-run succeeds, run `cargo publish --dry-run -p beamr`.

## Validation gates

- [ ] Run `cargo check`.
- [ ] Run `cargo test --package beamr --lib`.
- [ ] Run `cargo test --package beamr --test '*'`.
- [ ] Run `cargo test --package beamr --features differential --test differential`.
- [ ] Run `cargo bench --package beamr --no-run`.
- [ ] Run `cargo clippy --package beamr --all-targets`.
- [ ] Run `cargo doc --package beamr --no-deps`.

## Release constraints

- [ ] Audit non-test production source for `eprintln!`, `dbg!`, `todo!(`, `unimplemented!(`, and unacceptable `panic!(`.
- [ ] Confirm no known GC safety bugs are open and that the B-165 GC fix is merged into the release branch/mainline.
- [ ] Create local tag `v0.4.0` only after all validation gates pass.
- [ ] Do not push `v0.4.0` until project-lead approval is recorded.
- [ ] Do not run `cargo publish` without project-lead approval.
