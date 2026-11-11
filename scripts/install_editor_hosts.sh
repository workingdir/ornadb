#!/usr/bin/env bash
# Installs the editor packages the Gate E and highlighter suites run against,
# then checks that each package's binary is on PATH.
#
# Usage: install_editor_hosts.sh PACKAGE...
#
# A dead apt mirror must not stop the install, and a stalled download must not
# hang the job. Every network call is capped, and the steps are:
#   1. `apt-get update` on the runner's own sources, two attempts, each capped at
#      60 s. If both fail, a warning is logged and the run continues.
#   2. If that update failed, one `apt-get update` against the Ubuntu archive
#      (http://archive.ubuntu.com/ubuntu/), capped at 60 s. The archive is reached
#      through rewritten copies of the sources in a temporary directory, so
#      /etc/apt is never modified. If this also fails, the run continues with the
#      package lists already on the runner.
#   3. Download the packages, three attempts, each capped at 150 s. apt keeps
#      partial .deb files, so a retry resumes them. Before attempts 2 and 3 the
#      archive switches to the Ubuntu archive (once per run) and the lists are
#      refreshed. A failed attempt prints the tail of apt's HTTP trace, and every
#      attempt prints which archive hosts it contacted. A failed download is not
#      fatal: step 4 fetches whatever is still missing.
#   4. Install the packages, capped at 150 s. This is the only fatal apt step.
#   5. Verify that every package's binary is on PATH; fail naming the first one
#      that is missing.
set -euo pipefail

if (($# == 0)); then
  echo "usage: $0 PACKAGE..." >&2
  exit 2
fi

apt_opts=(-o Acquire::Retries=3 -o Acquire::ForceIPv4=true -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30)
# INSTALL_EDITOR_APT_ROOT lets the self-test point at a fixture tree; CI never sets it.
apt_root=${INSTALL_EDITOR_APT_ROOT:-/etc/apt}
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT

# Empty array = the runner's own sources. Set by select_mirror to the fallback
# override when the Ubuntu archive is selected.
mirror_opts=()
current_mirror=primary
fallback_list=
fallback_parts=
fallback_tried=0
fallback_ready=0

warn() {
  echo "::warning::$*"
}

# retry ATTEMPTS COMMAND...: runs COMMAND up to ATTEMPTS times. Returns 1 without
# printing an error, so the caller decides whether the failure is fatal.
retry() {
  local attempts=$1 n
  shift
  for ((n = 1; n <= attempts; n++)); do
    if "$@"; then
      return 0
    fi
    warn "Attempt $n of $attempts failed: $*"
    if ((n < attempts)); then
      sleep 5
    fi
  done
  return 1
}

refresh_lists() {
  echo "Refreshing package lists from the $current_mirror mirror"
  sudo timeout -k 10 60 apt-get "${apt_opts[@]}" "${mirror_opts[@]}" update
}

# Copies one sources file, pointing the Azure archive host and the Ubuntu mirror
# list (mirror+file:/etc/apt/apt-mirrors.txt, used by the runner images) at the
# Ubuntu archive.
rewrite_mirror() {
  sed -e 's|mirror+file:/etc/apt/apt-mirrors\.txt|http://archive.ubuntu.com/ubuntu/|g' \
    -e 's|azure\.archive\.ubuntu\.com|archive.ubuntu.com|g' "$1" >"$2"
}

make_fallback_sources() {
  local f
  fallback_list=$work_dir/fallback/sources.list
  fallback_parts=$work_dir/fallback/sources.list.d
  mkdir -p "$fallback_parts"
  : >"$fallback_list"
  if [[ -f $apt_root/sources.list ]]; then
    rewrite_mirror "$apt_root/sources.list" "$fallback_list"
  fi
  for f in "$apt_root"/sources.list.d/*; do
    [[ -f $f ]] || continue
    rewrite_mirror "$f" "$fallback_parts/${f##*/}"
  done
}

# select_mirror primary|fallback: chooses which sources the apt calls use. The
# fallback is only selected once its lists have refreshed, so the install never
# reads a list set that was not fetched.
select_mirror() {
  if [[ $1 == fallback ]]; then
    mirror_opts=(-o "Dir::Etc::SourceList=$fallback_list" -o "Dir::Etc::SourceParts=$fallback_parts")
    current_mirror=archive.ubuntu.com
  else
    mirror_opts=()
    current_mirror=primary
  fi
}

# Tries the Ubuntu archive. Its lists are refreshed at most once per run; later
# calls only re-select the archive if that refresh worked. Returns 0 when the
# archive is selected, otherwise the primary sources are selected and it returns 1.
switch_to_fallback() {
  if ((fallback_ready)); then
    select_mirror fallback
    return 0
  fi
  if ((fallback_tried)); then
    select_mirror primary
    return 1
  fi
  fallback_tried=1
  make_fallback_sources
  select_mirror fallback
  if refresh_lists; then
    fallback_ready=1
    return 0
  fi
  select_mirror primary
  return 1
}

# Prints how many trace lines named each host in the log file $1.
summarise_hosts() {
  grep -oE 'https?://[^/ "]+' "$1" | sort | uniq -c || true
}

download_packages() {
  local n log="$work_dir/download.log"
  for ((n = 1; n <= 3; n++)); do
    if ((n > 1)); then
      warn "Download attempt $((n - 1)) failed; switching to the Ubuntu archive and refreshing lists"
      if ! switch_to_fallback; then
        warn "Ubuntu archive lists unavailable; downloading from the $current_mirror mirror"
      fi
    fi
    echo "Download attempt $n of 3 from the $current_mirror mirror"
    if sudo timeout -k 10 150 apt-get "${apt_opts[@]}" "${mirror_opts[@]}" -o Debug::Acquire::http=true install -y --download-only --no-install-recommends "$@" >"$log" 2>&1; then
      echo "Download attempt $n of 3 succeeded; archive hosts in apt trace:"
      summarise_hosts "$log"
      return 0
    fi
    warn "Download attempt $n of 3 failed; archive hosts in apt trace:"
    summarise_hosts "$log"
    warn "Last 40 lines of the apt HTTP trace:"
    tail -n 40 "$log"
    if ((n < 3)); then
      sleep 5
    fi
  done
  warn "Download failed after 3 attempts; the install step will fetch what is still missing"
  return 1
}

# Package name -> binary the suites run. Packages not listed here ship a binary
# with the same name.
binary_for() {
  case $1 in
    emacs-nox) echo emacs ;;
    *) echo "$1" ;;
  esac
}

if retry 2 refresh_lists; then
  echo "apt-get update succeeded on the runner's default archive"
else
  warn "apt-get update failed after 2 attempts on the runner's default archive; trying archive.ubuntu.com once"
  if switch_to_fallback; then
    echo "apt-get update succeeded on archive.ubuntu.com"
  else
    warn "apt-get update failed on archive.ubuntu.com as well; installing from the package lists already on the runner"
  fi
fi

# A failed download is not fatal: the install below is the step that must succeed.
download_packages "$@" || true

if ! sudo timeout -k 10 150 apt-get "${apt_opts[@]}" "${mirror_opts[@]}" install -y --no-install-recommends "$@"; then
  echo "::error::apt-get install failed for: $* (mirror: $current_mirror)"
  exit 1
fi

for pkg in "$@"; do
  bin=$(binary_for "$pkg")
  if ! command -v "$bin" >/dev/null 2>&1; then
    echo "::error::Required binary '$bin' (package $pkg) is not on PATH after install"
    exit 1
  fi
done
echo "Verified binaries on PATH for: $*"
