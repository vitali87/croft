# Installing croft

Every way to get croft onto a machine: Homebrew, prebuilt binaries through cargo or straight
from a release, and building from source. The [README](../README.md#install) keeps the short
version; what croft needs from your system is in its [Requirements](../README.md#requirements).

## Homebrew

With [Homebrew](https://brew.sh) on macOS or Linux, no toolchain and no compile:

```bash
brew install vitali87/croft/croft
```

Upgrade with `brew upgrade croft`; croft's own updater (`Ctrl+Shift+F9`) leaves a Homebrew
install to brew. Each release updates the formula, and it installs the same
prebuilt archives as below.

## Prebuilt binaries through cargo

```bash
cargo binstall croft-software
```

`cargo binstall` itself needs installing once (`cargo install cargo-binstall`,
or a prebuilt from [its releases](https://github.com/cargo-bins/cargo-binstall#installation)).

That downloads the tagged release for your platform (macOS arm64/x86_64, Linux
musl arm64/x86_64, Android arm64 in Termux) and puts `croft` on `PATH` in
seconds.

## From the latest release

With nothing but `curl` and `tar`:

```bash
os=$([ "$(uname -s)" = Darwin ] && echo apple-darwin || echo unknown-linux-musl)
arch=$(uname -m | sed 's/arm64/aarch64/')
curl -fsSL "https://github.com/vitali87/croft/releases/latest/download/croft-$arch-$os.tar.gz" | tar xz
sudo mkdir -p /usr/local/bin
sudo mv "croft-$arch-$os/croft" /usr/local/bin/
```

`/usr/local/bin` is on the default `PATH` but may not exist yet on a fresh Mac,
hence the `mkdir`. On Android in Termux, set `os=linux-android` for the
Android build, and as there is no `sudo` and no `/usr/local/bin`, move it to
`"$PREFIX/bin/"` instead ([ANDROID.md](ANDROID.md#install)).

### Verifying the archive

To verify the archive before installing it, download it to a file instead of
piping it into `tar`, together with the release's `SHA256SUMS` and that file's
signature. `SHA256SUMS` lists all five archives, so check only the one you
fetched rather than running it whole:

```bash
os=$([ "$(uname -s)" = Darwin ] && echo apple-darwin || echo unknown-linux-musl)
arch=$(uname -m | sed 's/arm64/aarch64/')
tag=$(curl -fsSLo /dev/null -w '%{url_effective}' https://github.com/vitali87/croft/releases/latest | sed 's|.*/||')
for f in "croft-$arch-$os.tar.gz" SHA256SUMS SHA256SUMS.sigstore.json; do
  curl -fsSLO "https://github.com/vitali87/croft/releases/download/$tag/$f"
done
grep "croft-$arch-$os.tar.gz" SHA256SUMS | sha256sum -c -
# macOS: grep "croft-$arch-$os.tar.gz" SHA256SUMS | shasum -a 256 -c -
```

`SHA256SUMS` is itself signed with [Sigstore](https://www.sigstore.dev/) by the
release workflow, keyless, so the signature proves the file came from this
repository's `release.yml` run for that tag. Check it with
[cosign](https://docs.sigstore.dev/cosign/system_config/installation/) before
trusting the sums, then install the archive you verified:

```bash
cosign verify-blob \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-identity "https://github.com/vitali87/croft/.github/workflows/release.yml@refs/tags/$tag" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  SHA256SUMS
tar xzf "croft-$arch-$os.tar.gz"
sudo mkdir -p /usr/local/bin
sudo mv "croft-$arch-$os/croft" /usr/local/bin/
```

## Building from source

Building needs Rust and a C compiler; see [Install Rust](#install-rust) below.

```bash
cargo install croft-software --locked
```

This compiles the latest [crates.io](https://crates.io/crates/croft-software) release into `~/.cargo/bin/croft`. Re-run to upgrade. To track `main` instead:

```bash
cargo install --git https://github.com/vitali87/croft.git --locked
```

Or to build from a source checkout:

```bash
git clone https://github.com/vitali87/croft.git
cd croft
cargo install --path . --locked
```

**macOS:** the build directory churns the Spotlight index and can spike your CPU and fans. Before building, point Cargo at a `.noindex` directory Spotlight ignores; see [MACOS.md](MACOS.md#spotlight-indexing-and-the-build-directory).

### Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then open a new terminal — or run `. "$HOME/.cargo/env"` in the current one — so `cargo` is on `PATH`.

rustup does not install the C compiler and linker the build needs. On macOS run
`xcode-select --install`; on Debian or Ubuntu, `sudo apt install build-essential`
(or your distribution's equivalent).
