# Releasing herdr-tilt

GitHub Actions publishes prebuilt binaries for the four supported platform and
architecture combinations. A Herdr installation downloads one of these files,
verifies its adjacent SHA-256 checksum, and installs it at
`target/release/herdr-tilt` inside Herdr's managed plugin checkout.

## Prepare a release

1. Update the version in `Cargo.toml` and `herdr-plugin.toml` to the same
   semantic version, then run `cargo check` so `Cargo.lock` is current.
2. Add the user-visible changes to the release notes or commit history.
3. Run the local checks:

   ```sh
   cargo fmt --check
   cargo clippy --all-targets --locked -- -D warnings
   cargo test --locked
   cargo build --release --locked
   ./scripts/check-release-version.sh v0.1.1
   ```

   Replace `v0.1.1` with the version being released.

4. Commit the version change, create the matching tag, and push it:

   ```sh
   git tag v0.1.1
   git push origin v0.1.1
   ```

The `Release` workflow rejects a tag that does not match both metadata files.
After validation, it builds these assets:

- `herdr-tilt-macos-aarch64`
- `herdr-tilt-macos-x86_64`
- `herdr-tilt-linux-aarch64`
- `herdr-tilt-linux-x86_64`

Each binary has a sibling `<asset>.sha256` file. The workflow creates the
GitHub release only after every build and checksum is present.

## Verify the published release

Test from a machine without a Rust toolchain in the effective `PATH`, or from a
clean user account:

```sh
herdr plugin install the-inconvenience-store/herdr-tilt --ref v0.1.1
herdr plugin list --plugin herdr.tilt
```

Open the dashboard from a workspace containing a `Tiltfile` and verify that it
can connect to Tilt. Repeat the install on at least one macOS machine and one
Linux machine. The installer fails without replacing an existing binary if an
asset is unavailable or its checksum does not match.

## Marketplace discovery

The public GitHub repository must have the `herdr-plugin` topic. Herdr's
marketplace discovers public repositories from that topic; no separate package
submission is required.
