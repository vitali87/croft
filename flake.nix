{
  description = "croft — reproducible dev shell unifying the macOS and Linux toolchains";

  inputs = {
    # Pinned to a release where the classic `darwin.apple_sdk.frameworks.*`
    # attributes still exist (croft links AppKit/objc). Bump deliberately; on a
    # much newer nixpkgs the Apple SDK is propagated by stdenv and the explicit
    # framework list below may need trimming.
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.11";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        inherit (pkgs) lib stdenv;

        # The Rust version comes from rust-toolchain.toml, so this shell and a
        # bare `rustup`-driven `cargo` agree exactly — no drift between machines.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

        # Clipboard backends the Linux clipboard tests + runtime shell out to.
        # With these on PATH `cargo test` exercises the real wl-copy/xclip/xsel
        # round-trip instead of skipping (croft falls back to OSC 52 otherwise).
        linuxClipboard = lib.optionals stdenv.isLinux [
          pkgs.wl-clipboard
          pkgs.xclip
          pkgs.xsel
        ];

        # croft's terminal clipboard FFI links objc + AppKit; notify uses
        # CoreServices (FSEvents). Linux needs none of these.
        darwinFrameworks = lib.optionals stdenv.isDarwin (with pkgs.darwin.apple_sdk.frameworks; [
          AppKit
          Cocoa
          CoreServices
          CoreFoundation
          Security
        ]);
      in
      {
        devShells.default = pkgs.mkShell {
          # cc (for the tree-sitter `cc`-built grammars) comes from stdenv.
          nativeBuildInputs = [ pkgs.pkg-config ];

          buildInputs = [ rustToolchain ] ++ linuxClipboard ++ darwinFrameworks;

          shellHook = ''
            # Cap test threads AND build jobs at half the cores so a run never
            # pins a shared machine and starves other apps — the committed,
            # cross-machine version of the old gitignored .cargo/config.toml.
            # This is the same (cores+1)/2 policy croft's installer uses for
            # build jobs (src/remote.rs). Override per-run: `RUST_TEST_THREADS=N`.
            cores="$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 2)"
            half="$(( (cores + 1) / 2 ))"
            export RUST_TEST_THREADS="$half"
            export CARGO_BUILD_JOBS="$half"
            echo "croft dev shell · rust $(rustc --version | cut -d' ' -f2) · tests+build capped at $half/$cores cores (set RUST_TEST_THREADS/CARGO_BUILD_JOBS to override)"
          '';
        };

        # `nix fmt` formats the Nix files in this repo.
        formatter = pkgs.nixpkgs-fmt;
      });
}
