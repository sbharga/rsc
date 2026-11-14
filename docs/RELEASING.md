# Releasing

1. Bump `version` in `Cargo.toml`, then run `cargo build` to update `Cargo.lock`.
2. Add `## X.Y.Z` to `CHANGELOG.md`. Release notes come from it, and publishing fails without it.
3. Run `cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked`.
4. Commit and push `master`. Wait for CI to be green on that exact commit: `gh run list --commit "$(git rev-parse HEAD)"`, then `gh run watch <id>`.
5. Dry run: `gh workflow run release.yml --ref master`, then `gh run watch`. All 8 builds and `publish` must be green. A dry run builds, signs and verifies but uploads nothing to a release.
6. TLS smoke without plan usage: `cargo build --release && ./target/release/rsc update` prints `rsc X.Y.Z is up to date.` For a manual live API check, use `rsc run --preset quick --yes "<question>"`.
7. `gh release create vX.Y.Z --target master --title vX.Y.Z --notes "See CHANGELOG.md"`, plus `--prerelease` while releases stay pre-releases.
8. Watch the Release run. Check `gh release view vX.Y.Z --json assets -q '.assets[].name'` lists 26 files: for each of the 8 targets, the archive, `.sha256` and `.sigstore.json`, plus `install.sh` and `install.ps1`.
9. Verify one asset:
   ```sh
   cosign verify-blob rsc-x86_64-unknown-linux-musl.tar.gz --bundle rsc-x86_64-unknown-linux-musl.tar.gz.sigstore.json \
     --certificate-identity https://github.com/sbharga/rsc/.github/workflows/release.yml@refs/tags/vX.Y.Z \
     --certificate-oidc-issuer https://token.actions.githubusercontent.com
   ```
   Install with `RSC_VERSION=vX.Y.Z sh install.sh`.
10. The installer is pinned per tag: `rsc update` fetches `install.sh`/`install.ps1` from the new tag.

If the Release run fails after the tag exists, fix on `master`, then `gh release delete vX.Y.Z --yes --cleanup-tag` and recreate the release at the fixed commit. Only do this while nobody has downloaded the release.
