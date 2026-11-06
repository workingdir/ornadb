#!/usr/bin/env bash
# Installs the editor packages the Gate E and highlighter suites run against.
#
# Usage: install_editor_hosts.sh PACKAGE...
#
# Stages, so a stalled mirror fails fast and a killed download never leaves
# dpkg half-configured:
#   1. `apt-get update`, two attempts, each capped at 60 s.
#   2. Download the packages, three attempts, each capped at 150 s. apt keeps
#      partial .deb files, so a retry resumes them. Before attempts 2 and 3 the
#      archive host moves from azure.archive.ubuntu.com to archive.ubuntu.com
#      and the lists are refreshed. A failed attempt prints the tail of apt's
#      HTTP trace, and every attempt prints which archive hosts it contacted.
#   3. Install the packages from the downloaded files.
set -euo pipefail

if (($# == 0)); then
  echo "usage: $0 PACKAGE..." >&2
  exit 2
fi

apt_opts=(-o Acquire::Retries=3 -o Acquire::ForceIPv4=true -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30)
mirror_files=(/etc/apt/sources.list /etc/apt/sources.list.d/* /etc/apt/apt-mirrors.txt)

retry() {
  local attempts=$1 n
  shift
  for ((n = 1; n <= attempts; n++)); do
    if "$@"; then
      return 0
    fi
    echo "::warning::Attempt $n of $attempts failed: $*"
    sleep 5
  done
  echo "::error::Giving up after $attempts attempts: $*"
  return 1
}

switch_archive_host() {
  local f
  for f in "${mirror_files[@]}"; do
    [[ -f $f ]] || continue
    sudo sed -i 's|azure\.archive\.ubuntu\.com|archive.ubuntu.com|g' "$f"
  done
}

# Prints how many trace lines named each host in the log file $1.
summarise_hosts() {
  grep -oE 'https?://[^/ "]+' "$1" | sort | uniq -c || true
}

download_packages() {
  local n log
  log=$(mktemp)
  for ((n = 1; n <= 3; n++)); do
    if ((n > 1)); then
      echo "::warning::Download attempt $((n - 1)) failed; switching archive host and refreshing lists"
      switch_archive_host
      sudo timeout -k 10 60 apt-get "${apt_opts[@]}" update
    fi
    if sudo timeout -k 10 150 apt-get "${apt_opts[@]}" -o Debug::Acquire::http=true install -y --download-only --no-install-recommends "$@" >"$log" 2>&1; then
      echo "Download attempt $n of 3 succeeded; archive hosts in apt trace:"
      summarise_hosts "$log"
      rm -f "$log"
      return 0
    fi
    echo "::warning::Download attempt $n of 3 failed; archive hosts in apt trace:"
    summarise_hosts "$log"
    echo "::warning::Last 40 lines of the apt HTTP trace:"
    tail -n 40 "$log"
    sleep 5
  done
  echo "::error::Giving up after 3 attempts: apt-get install --download-only $*"
  return 1
}

retry 2 sudo timeout -k 10 60 apt-get "${apt_opts[@]}" update
download_packages "$@"
sudo apt-get "${apt_opts[@]}" install -y --no-install-recommends "$@"
