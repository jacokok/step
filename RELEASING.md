# Releasing step

Release automation lives in `.github/workflows/release.yml`. The README is for
installed-tool users; this file is for maintainers.

## Publish a release

1. Set the intended version in `Cargo.toml`, regenerate `Cargo.lock` with
   `mise exec -- cargo check`, and commit both. The first version is `0.1.0`.
2. Push the commit, including the release workflow, to `jacokok/step`.
3. In GitHub, open **Actions → Release step → Run workflow**.
4. Choose the branch/commit to release and enter the version **without `v`**
   (for example, `0.1.0`). It must match the package version.
5. The workflow tests, builds all platforms, and creates tag `vVERSION` and a
   GitHub release from the exact tested commit. It refuses to overwrite an
   existing tag. No manual tag push is necessary.

The workflow uses `GITHUB_TOKEN`; only its publishing job has `contents: write`.
Enable GitHub Actions for the repository. No additional publishing secret is needed.
If repository/organization policy blocks release creation or a runner platform,
allow it before dispatching. ARM Linux runners require a public repository or an
appropriate runner entitlement.

## Artifacts

- `step-vVERSION-aarch64-apple-darwin.tar.gz`
- `step-vVERSION-x86_64-apple-darwin.tar.gz`
- `step-vVERSION-aarch64-unknown-linux-musl.tar.gz`
- `step-vVERSION-x86_64-unknown-linux-musl.tar.gz`
- `step-vVERSION-x86_64-pc-windows-msvc.zip`
- `SHA256SUMS`

Each archive contains `step` (or `step.exe`), `README.md`, and `LICENSE` at its root.
Standard Rust target names allow mise's GitHub backend to select the correct archive
without custom asset patterns. Linux binaries use musl to avoid host glibc-version
requirements. macOS builds target macOS 11 or newer. Binaries are not code-signed.
Models, yt-dlp and FFmpeg are not bundled; users run `step models` once.

All platforms build with the toolchain pinned in `mise.toml` and locked dependencies.
The gating Linux job installs FFmpeg, downloads checksum-pinned small models, and
runs every test including the normally ignored CLI transport/resume test. Each
platform build smoke-tests `step --version` and CLI help before packaging. Artifacts
are published only if the entire matrix succeeds. Failed unpublished drafts/tags
must be inspected and cleaned up manually before retrying the same version.

## Local checks

```sh
mise install rust
mise run test
mise run lint
mise run build
./target/release/step models --small --dir models
mise exec -- cargo test --locked -- --include-ignored
```

The full CLI test reads `models/` in the repository, or `STEP_TEST_MODEL_DIR` if set.
Installed-tool defaults instead use the platform's user cache directory; override
that with `--dir`, `--model-dir`, or `STEP_MODEL_DIR`.

After publishing the first release, verify the actual distribution from an empty
folder (the install cannot work before release assets exist):

```sh
mise use github:jacokok/step
mise exec -- step --version
mise exec -- step models
```

Mise calls its install-and-configure command `mise use`; `mise add` is not supported
by the mise version used here.
