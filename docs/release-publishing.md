# Release And Publishing

Jikji releases use GitHub Actions for cross-OS binaries and crates.io trusted
publishing. The primary publish path does not use a long-lived `CRATES_IO_TOKEN`
secret.

## CI

`.github/workflows/ci.yml` runs on pull requests and pushes to `main` across
Ubuntu, macOS, and Windows. It checks formatting, clippy, `cargo test
--workspace --all-features`, non-Windows Python parity contract tests
(`python -m pytest python/jikji/tests tests/parity`), a Windows Python
compatibility smoke, `cargo doc` smoke, and a cross-OS CLI command smoke.

## GitHub Release Artifacts

`.github/workflows/release.yml` runs for `v*.*.*` tags and manual dispatches.
It builds and uploads:

- `x86_64-unknown-linux-gnu`
- `aarch64-apple-darwin`
- `x86_64-apple-darwin`
- `x86_64-pc-windows-msvc`

Each archive is produced by `scripts/release/build-artifacts.sh` and has a
matching `.sha256` file. Local maintainers can inspect the artifact shape
without compiling by running:

```bash
bash scripts/release/build-artifacts.sh --dry-run
```

## crates.io Trusted Publishing

`.github/workflows/publish.yml` is the primary crates.io publish path. It runs
automatically after pushes to `main`, and it also supports GitHub Release
publishing plus manual package verification through `workflow_dispatch`. It
grants `id-token: write`, uses `rust-lang/crates-io-auth-action@v1`, and passes
the temporary trusted-publishing token to `cargo publish` when crates.io issues
one. The workflow verifies the workspace package tarballs with real
`cargo package` verification before publishing. If crates.io has not yet been
configured to trust this repository, the workflow records a warning and exits
after package verification instead of failing the `main` branch. Release
publishing must run from a clean checkout; `--allow-dirty` is only for local
package-shape inspection while a change is still under review.

Before publishing, the workflow checks crates.io for each package/version pair.
Versions that already exist are skipped so ordinary `main` pushes after a
release do not fail by attempting to republish immutable crate versions.

Before the first publish, configure every publishable crate on crates.io with a
trusted publisher entry for this repository:

- repository owner/name: `NomaDamas/jikji`
- workflow file: `publish.yml`
- environment: `crates-io`
- crate list: `jikji-core`, `jikji-media-bridge`, `jikji-parser`,
  `jikji-search`, `jikji-agent`, `jikji-index`, `jikji-public-datasets`,
  `jikji-bench`, `jikji-hermes-bench`, `jikji-cli`

`jikji-cli` depends on `jikji-public-datasets`, `jikji-bench`, and
`jikji-hermes-bench`, so all three must be published before it. Only the
internal `jikji-parity` test crate has `publish = false` and is excluded.

## Local Publish Dry Runs Before First Release

`cargo package --workspace --exclude jikji-parity` is the
local package-shape gate and should pass before release. For publish dry-runs before the first
crates.io release, only the first dependency crate can be fully proven against
the live registry:

```bash
cargo publish --dry-run -p jikji-core --allow-dirty
```

Dependent crates such as `jikji-parser`, `jikji-search`, and `jikji-cli` depend
on the workspace versions of earlier Jikji crates. Until those versions exist in
the crates.io index, their local `cargo publish --dry-run` commands are expected
to fail with a registry dependency-resolution error. That failure is an external
publish-order blocker, not proof that the dependent crate package is invalid.
The trusted workflow publishes crates in dependency order so each later crate
sees the earlier crate version in the registry.

Token-secret publishing is only a manual break-glass fallback. If trusted
publishing is unavailable, a maintainer may run `cargo publish -p <crate>` from a
clean checkout with a scoped local cargo token, following the same crate order
listed above. Do not add a `CRATES_IO_TOKEN` secret to the trusted workflow.
