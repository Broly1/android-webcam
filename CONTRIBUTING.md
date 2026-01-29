# Developer Workflow

### Testing
1. Run `./build.sh`.
2. Select your distro.
3. Use the Git Hash (displayed in the script) to track your testing version.

### Releasing
1. Bump the version in `Cargo.toml`.
2. Run `cargo update` to sync the lock file.
3. Tag the commit: `git tag -a v1.x.x -m "Release message"`.
4. Run `./build.sh` to generate the final packages.
