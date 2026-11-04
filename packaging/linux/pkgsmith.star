# The Orna Linux packages.
#
# pkgsmith reads the install prefix that `package.sh build` staged from a real
# release build of `orna`, and writes one package per `package(...)` call below.
# It builds nothing and runs no command, so this file carries only what is true
# of the files themselves: `package.py` does the reproducible build and writes
# `usr/share/orna/distribution-manifest.toml` into the prefix.
#
# The artifact contract is unchanged from the 1.0.0 packager: exactly one
# executable and the distribution manifest that binds it to its build inputs,
# no package-manager lifecycle hooks, no network access, no runtime helper
# executable, and no external database engine. No call below passes `scripts`,
# so no format carries one.

VERSION = arg("VERSION", "unset")

COMMON = {
    "name": "orna",
    "version": VERSION,
    "description": """The Orna database command-line program.
OrnaDB is a Git-backed database platform for typed applications. orna reads, writes and queries Orna projects on the local machine, with no network, no daemon, no external database engine and no runtime helper executable.""",
    "arch": "amd64",
    # There is no published maintainer address yet. `.invalid` is reserved for
    # exactly this, so a placeholder can never be mistaken for a real contact.
    "maintainer": "OrnaDB maintainers <maintainers@ornadb.invalid>",
    "homepage": "https://github.com/workingdir/ornadb",
    "license": "Apache-2.0",
    # The two payload paths are the whole inventory, so the artifact owns no
    # directory. The formats that need parent entries to unpack (deb, rpm) add
    # them as root:root 0755 themselves.
    "own_dirs": [],
}

# The install artifact of release 1.0.0: an uncompressed tar of the two
# root-owned payload files, with every timestamp taken from SOURCE_DATE_EPOCH.
package(
    "tar",
    options = {"compression": "none"},
    file = "{name}-{version}-linux-amd64.tar",
    **COMMON
)

# Fedora, openSUSE and RHEL. The dynamic Rust binary links `libc`/`libm` from
# glibc and `libgcc_s` from libgcc; the other requirements rpm records come
# from its own format.
package(
    "rpm",
    depends = ["glibc", "libgcc"],
    options = {"group": "Applications/Databases"},
    **COMMON
)

# Debian and Ubuntu. `libgcc_s` is packaged as `libgcc-s1` from Debian 11.
package(
    "deb",
    depends = ["libc6", "libgcc-s1"],
    options = {"section": "database"},
    **COMMON
)
