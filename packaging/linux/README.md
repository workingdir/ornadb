# Orna Linux packages

`package.sh` builds the release executable once, installs it into an install
prefix, and lets [pkgsmith](https://github.com/kierandrewett/pkgsmith) write
every package from one manifest, [`pkgsmith.star`](pkgsmith.star). pkgsmith
builds nothing and runs no command; `package.py` supplies the two things the
artifact contract needs and pkgsmith does not do — a reproducible release build
of the one executable, and the distribution manifest that binds it to its build
inputs.

| Format | File | Installed with |
| ------ | ---- | -------------- |
| `tar`  | `orna-<version>-linux-amd64.tar` | `package.sh install`, or unpacking it |
| `rpm`  | `orna-<version>-1.x86_64.rpm` | `rpm -i` on Fedora, openSUSE, RHEL |
| `deb`  | `orna_<version>-1_amd64.deb` | `dpkg -i` on Debian, Ubuntu |

## Requirements

pkgsmith 0.1.1 or newer, with its format plugins beside the binary:

```sh
git clone https://github.com/kierandrewett/pkgsmith
cd pkgsmith
cargo build --release --workspace
export PKGSMITH=$PWD/target/release/pkgsmith
```

`package.py` uses `$PKGSMITH` when it is set and `pkgsmith` on `PATH`
otherwise. `verify` also needs `rpm` (to read an rpm) and `dpkg-deb` (to read a
deb); `install` needs nothing but Python.

## Commands

```sh
packaging/linux/package.sh build    # build orna, stage the prefix, write dist/
packaging/linux/package.sh verify   # re-verify the packages in dist/
packaging/linux/package.sh install <tar> [--root DIR]
packaging/linux/package.sh test     # packaging/linux/test_package.py
```

`build` accepts `--executable PATH` (package an existing Linux x86_64
executable instead of running Cargo), `--output-dir DIR`, and
`--source-date-epoch EPOCH`. It prints the pkgsmith plan before writing, and
verifies every package it wrote; a failed check leaves the packages in `dist/`
and exits non-zero.

Outputs are package-owned state under `target/linux-package/`: `cargo/` (the
release build), `prefix/` (the staged payload), and `dist/` (the packages). The
whole directory is rebuilt on every `build`, so no stale input can reach a
package. `dist/.source-date-epoch` records the epoch that build used, which is
how a later `verify` checks every timestamp; pass `--source-date-epoch` or set
`SOURCE_DATE_EPOCH` to check another value, or to verify a `dist/` from
elsewhere. With none of the three, a package set is checked for one consistent
timestamp but not for its exact value, and `verify` says so.

## What the artifact is

Exactly two installed files:

- `usr/bin/orna` — the executable, root:root, mode 0755;
- `usr/share/orna/distribution-manifest.toml` — the format-1 distribution
  manifest, root:root, mode 0644.

The manifest binds the installed executable's SHA-256 to `rustc`, `cargo`, the
`Cargo.lock` digest, the workspace path remap, and the link flag that removes
the build id. No format carries a package-manager lifecycle hook, a
configuration file, a dependency the executable does not already link against,
or a network access. There is one architecture: `amd64`.

The tar carries no directory entries. The deb adds `usr/` and `usr/share/orna/`
as root:root 0755 because dpkg needs them to unpack; the rpm carries none.

Nothing here is signed. pkgsmith can sign a package with a key it is given, and
no key is given: signing is the release authority's step, not the build's.

## Determinism

Two builds from the same inputs write identical bytes for all three formats.
`SOURCE_DATE_EPOCH` (default `0`) is the only clock: pkgsmith stamps every file
time from it, and the build passes it to Cargo. The build also refuses ambient
compiler overrides (`RUSTFLAGS`, `RUSTC*`, `CARGO_PROFILE_*`,
`CARGO_TARGET_*_RUSTC|LINKER|AR`, …), compiles with
`--remap-path-prefix=<repository>=/usr/src/orna` and
`-C link-arg=-Wl,--build-id=none`, and turns incremental compilation off, so
the executable itself does not depend on where or when it was built.

`test_package.py` proves the package half of that: it builds the whole package
set twice and compares every byte.

## What changed from the 1.0.0 packager

The 1.0.0 `package.py` wrote the tar itself with `tarfile`. pkgsmith writes it
now, along with the rpm and deb, from `pkgsmith.star`.

- The tar keeps its name, its two-file inventory, its modes, its root ownership,
  and its `SOURCE_DATE_EPOCH` timestamps. Only the tar header dialect differs:
  pkgsmith writes GNU-style headers where `tarfile` wrote POSIX ustar. Every
  reader tested (`tar`, Python `tarfile`, `bsdtar`) accepts both.
- Nothing declares a maintainer today, so the packages carry
  `OrnaDB maintainers <maintainers@ornadb.invalid>`. `.invalid` is reserved for
  exactly this. Replace it in `pkgsmith.star` when the project has a real
  address.
- `rpm` and `deb` name the two libraries the dynamic binary already links
  (`glibc`+`libgcc`, `libc6`+`libgcc-s1`); the tar declares nothing, as before.
  `verify` requires that exact requirement set, so a stray dependency fails the
  build.
- `verify` now checks a directory of packages rather than one tar: it asks
  pkgsmith what the manifest declares, then proves the three files exist, carry
  the same payload bytes, keep the modes and owners, carry no lifecycle
  script, record the same timestamp, and — for the rpm — that `rpm -K` still
  matches its own digests.
- `install` is unchanged, and still installs the tar only. Installing the rpm
  or deb is the package manager's job.
