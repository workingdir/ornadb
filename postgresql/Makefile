SHELL := /bin/bash
.SHELLFLAGS := -euo pipefail -c
.DEFAULT_GOAL := help
.NOTPARALLEL:

REPOSITORY_ROOT := $(abspath $(CURDIR)/..)
UPSTREAM_SOURCE := $(REPOSITORY_ROOT)/third_party/postgresql
OVERLAY_ROOT := $(CURDIR)/overlays/18.4
PATCH_ROOT := $(CURDIR)/patches/18.4
CONTAINERFILE := $(CURDIR)/Containerfile
SUPPORT_BUILDER := $(CURDIR)/support_bundle.py
MANIFEST_BUILDER := $(CURDIR)/engine_manifest.py
LIFECYCLE_PROBE := $(CURDIR)/lifecycle_probe.c
LIFECYCLE_VERIFIER := $(CURDIR)/verify_lifecycle.py
IMAGE := orna-postgresql-engine:debian12-amd64-1
SOURCE_DATE_EPOCH := 1778528675
LIFECYCLE_UID := 61018
LIFECYCLE_GID := 61018
LIFECYCLE_PORT := 65432

CONFIGURE_FLAGS := \
	--build=x86_64-linux-gnu \
	--host=x86_64-linux-gnu \
	--disable-nls \
	--disable-rpath \
	--disable-debug \
	--disable-profiling \
	--disable-coverage \
	--disable-dtrace \
	--disable-tap-tests \
	--disable-injection-points \
	--disable-cassert \
	--without-tcl \
	--without-perl \
	--without-python \
	--without-gssapi \
	--without-pam \
	--without-bsd-auth \
	--without-ldap \
	--without-bonjour \
	--without-selinux \
	--without-systemd \
	--without-readline \
	--without-liburing \
	--without-libcurl \
	--without-libnuma \
	--without-libxml \
	--without-libxslt \
	--without-lz4 \
	--without-zstd \
	--without-icu \
	--without-llvm \
	--with-zlib

.PHONY: help verify-inputs prepare-source build-image archives support-data lifecycle verify-lifecycle manifest update-source

help:
	@printf '%s\n' \
		'PostgreSQL embedded-engine build' \
		'' \
		'  make -C postgresql verify-inputs' \
		'  make -C postgresql prepare-source TARGET_ROOT=<absolute-repo-target-path>' \
		'  make -C postgresql archives TARGET_ROOT=<absolute-repo-target-path>' \
		'  make -C postgresql support-data TARGET_ROOT=<absolute-repo-target-path>' \
		'  make -C postgresql lifecycle TARGET_ROOT=<absolute-repo-target-path>' \
		'  make -C postgresql verify-lifecycle TARGET_ROOT=<absolute-repo-target-path>' \
		'  make -C postgresql manifest TARGET_ROOT=<absolute-repo-target-path>' \
		'  make -C postgresql update-source POSTGRESQL_REF=<full-commit>'

verify-inputs:
	@expected_gitlink="$$(git -C "$(REPOSITORY_ROOT)" ls-files --stage -- third_party/postgresql | \
		awk '$$1 == "160000" { print $$2 }')"; \
	actual_commit="$$(git -C "$(UPSTREAM_SOURCE)" rev-parse --verify HEAD)"; \
	[[ "$${expected_gitlink}" =~ ^[0-9a-f]{40}$$ ]] || { \
		printf '%s\n' '[postgresql] error: the upstream gitlink is absent' >&2; exit 1; }; \
	[[ "$${actual_commit}" == "$${expected_gitlink}" ]] || { \
		printf '%s\n' '[postgresql] error: the upstream checkout does not match the gitlink' >&2; exit 1; }; \
	[[ -z "$$(git -C "$(UPSTREAM_SOURCE)" status --porcelain=v1 --untracked-files=all)" ]] || { \
		printf '%s\n' '[postgresql] error: the upstream checkout is not clean' >&2; exit 1; }; \
	git -C "$(REPOSITORY_ROOT)" ls-files --error-unmatch -- \
		postgresql/Makefile postgresql/Containerfile \
		postgresql/support_bundle.py postgresql/engine_manifest.py postgresql/lifecycle_probe.c \
		postgresql/verify_lifecycle.py \
		postgresql/overlays/18.4 postgresql/patches/18.4 >/dev/null; \
	[[ -z "$$(git -C "$(REPOSITORY_ROOT)" ls-files --others --exclude-standard -- \
		postgresql/Makefile postgresql/Containerfile \
		postgresql/support_bundle.py postgresql/engine_manifest.py postgresql/lifecycle_probe.c \
		postgresql/verify_lifecycle.py \
		postgresql/overlays/18.4 postgresql/patches/18.4)" ]] || { \
		printf '%s\n' '[postgresql] error: the integration input contains an untracked file' >&2; exit 1; }; \
	git -C "$(REPOSITORY_ROOT)" diff --quiet -- \
		postgresql/Makefile postgresql/Containerfile \
		postgresql/support_bundle.py postgresql/engine_manifest.py postgresql/lifecycle_probe.c \
		postgresql/verify_lifecycle.py \
		postgresql/overlays/18.4 postgresql/patches/18.4 || { \
		printf '%s\n' '[postgresql] error: the integration input differs from the Git index' >&2; exit 1; }; \
	[[ -z "$$(find "$(OVERLAY_ROOT)" -mindepth 1 ! -type d ! -type f -print -quit)" ]] || { \
		printf '%s\n' '[postgresql] error: an overlay input is not a regular file' >&2; exit 1; }; \
	overlay_count=0; \
	while IFS= read -r -d '' overlay; do \
		relative="$${overlay#$(OVERLAY_ROOT)/}"; \
		[[ "$$(stat -c '%a' "$${overlay}")" == 644 ]] || { \
			printf '%s\n' "[postgresql] error: overlay mode is not 0644: $${relative}" >&2; exit 1; }; \
		[[ ! -e "$(UPSTREAM_SOURCE)/$${relative}" ]] || { \
			printf '%s\n' "[postgresql] error: overlay replaces upstream path: $${relative}" >&2; exit 1; }; \
		! grep -Fq -- " b/$${relative}" "$(PATCH_ROOT)"/*.patch || { \
			printf '%s\n' "[postgresql] error: patch also owns overlay path: $${relative}" >&2; exit 1; }; \
		overlay_count=$$((overlay_count + 1)); \
	done < <(find "$(OVERLAY_ROOT)" -type f -print0 | LC_ALL=C sort -z); \
	[[ "$${overlay_count}" -gt 0 ]] || { \
		printf '%s\n' '[postgresql] error: the overlay inventory is empty' >&2; exit 1; }; \
	[[ -z "$$(find "$(PATCH_ROOT)" -mindepth 1 -maxdepth 1 ! -type f -print -quit)" ]] || { \
		printf '%s\n' '[postgresql] error: the patch inventory contains a non-regular entry' >&2; exit 1; }; \
	patch_number=1; \
	while IFS= read -r patch_path; do \
		patch_name="$${patch_path##*/}"; \
		expected_prefix="$$(printf '%04d-' "$${patch_number}")"; \
		[[ "$${patch_name}" =~ ^$${expected_prefix}[a-z0-9][a-z0-9-]*\.patch$$ ]] || { \
			printf '%s\n' "[postgresql] error: patch order is not contiguous at $${patch_name}" >&2; exit 1; }; \
		[[ "$$(stat -c '%a' "$${patch_path}")" == 644 ]] || { \
			printf '%s\n' "[postgresql] error: patch mode is not 0644: $${patch_name}" >&2; exit 1; }; \
		! grep -Eq '^(---|\+\+\+) /dev/null$$|^(new|deleted) file mode ' "$${patch_path}" || { \
			printf '%s\n' "[postgresql] error: patch changes the file inventory: $${patch_name}" >&2; exit 1; }; \
		diff_count=0; \
		while read -r old_path new_path; do \
			old_path="$${old_path#a/}"; new_path="$${new_path#b/}"; \
			[[ "$${old_path}" == "$${new_path}" && -f "$(UPSTREAM_SOURCE)/$${old_path}" ]] || { \
				printf '%s\n' "[postgresql] error: patch does not change one existing path: $${patch_name}" >&2; exit 1; }; \
			diff_count=$$((diff_count + 1)); \
		done < <(awk '$$1 == "diff" && $$2 == "-U0" && $$3 == "-Nr" { print $$4, $$5 }' "$${patch_path}"); \
		[[ "$${diff_count}" -gt 0 && "$${diff_count}" -eq "$$(grep -c '^diff ' "$${patch_path}")" ]] || { \
			printf '%s\n' "[postgresql] error: patch header is not accepted: $${patch_name}" >&2; exit 1; }; \
		patch_number=$$((patch_number + 1)); \
	done < <(find "$(PATCH_ROOT)" -mindepth 1 -maxdepth 1 -type f -name '*.patch' -print | LC_ALL=C sort); \
	[[ "$${patch_number}" -gt 1 ]] || { \
		printf '%s\n' '[postgresql] error: the patch inventory is empty' >&2; exit 1; }; \
	printf '%s\n' "[postgresql] inputs valid at $${actual_commit}"

prepare-source: verify-inputs
	@target_root='$(TARGET_ROOT)'; \
	[[ "$${target_root}" == "$(REPOSITORY_ROOT)/target/"* ]] || { \
		printf '%s\n' '[postgresql] error: TARGET_ROOT must be an absolute path below the repository target directory' >&2; exit 1; }; \
	mkdir -p -- "$${target_root}"; \
	target_root="$$(cd "$${target_root}" && pwd -P)"; \
	candidate="$${target_root}/.prepared-source.$$$$"; \
	inventory_candidate="$${target_root}/.prepared-source-inventory.$$$$"; \
	[[ ! -e "$${candidate}" && ! -e "$${inventory_candidate}" ]] || exit 1; \
	trap 'rm -rf -- "$${candidate}"; rm -f -- "$${inventory_candidate}"' EXIT; \
	mkdir -m 0700 -- "$${candidate}"; \
	git -C "$(UPSTREAM_SOURCE)" archive --format=tar HEAD | tar -xf - -C "$${candidate}"; \
	while IFS= read -r -d '' overlay; do \
		relative="$${overlay#$(OVERLAY_ROOT)/}"; \
		install -D -m 0644 -- "$${overlay}" "$${candidate}/$${relative}"; \
	done < <(find "$(OVERLAY_ROOT)" -type f -print0 | LC_ALL=C sort -z); \
	while IFS= read -r patch_path; do \
		patch_name="$${patch_path##*/}"; \
		dry_output="$$(patch --batch --forward --fuzz=0 --strip=1 --directory="$${candidate}" \
			--dry-run < "$${patch_path}" 2>&1)" || { printf '%s\n' "$${dry_output}" >&2; exit 1; }; \
		! grep -Eqi 'offset|fuzz' <<<"$${dry_output}" || { \
			printf '%s\n' "[postgresql] error: patch dry-run used offset or fuzz: $${patch_name}" >&2; exit 1; }; \
		apply_output="$$(patch --batch --forward --fuzz=0 --strip=1 --directory="$${candidate}" \
			< "$${patch_path}" 2>&1)" || { printf '%s\n' "$${apply_output}" >&2; exit 1; }; \
		! grep -Eqi 'offset|fuzz' <<<"$${apply_output}" || { \
			printf '%s\n' "[postgresql] error: patch application used offset or fuzz: $${patch_name}" >&2; exit 1; }; \
	done < <(find "$(PATCH_ROOT)" -mindepth 1 -maxdepth 1 -type f -name '*.patch' -print | LC_ALL=C sort); \
	find "$${candidate}" -exec touch -h -d '@$(SOURCE_DATE_EPOCH)' -- {} +; \
	while IFS= read -r -d '' path; do \
		relative="$${path#$${candidate}/}"; \
		printf '%s %s %s\n' "$$(stat -c '%a' "$${path}")" \
			"$$(sha256sum "$${path}" | awk '{ print $$1 }')" "$${relative}"; \
	done < <(find "$${candidate}" -type f -print0 | LC_ALL=C sort -z) > "$${inventory_candidate}"; \
	upstream_count="$$(git -C "$(UPSTREAM_SOURCE)" ls-tree -r --name-only HEAD | wc -l)"; \
	overlay_count="$$(find "$(OVERLAY_ROOT)" -type f | wc -l)"; \
	prepared_count="$$(wc -l < "$${inventory_candidate}")"; \
	[[ "$${prepared_count}" -eq $$((upstream_count + overlay_count)) ]] || { \
		printf '%s\n' '[postgresql] error: prepared source file count is not accepted' >&2; exit 1; }; \
	rm -rf -- "$${target_root}/prepared-source"; \
	rm -f -- "$${target_root}/prepared-source-inventory.txt"; \
	mv -- "$${candidate}" "$${target_root}/prepared-source"; \
	mv -- "$${inventory_candidate}" "$${target_root}/prepared-source-inventory.txt"; \
	trap - EXIT; \
	printf '%s\n' "[postgresql] prepared $${prepared_count} files below $${target_root}"

build-image:
	@target_root='$(TARGET_ROOT)'; \
	[[ "$${target_root}" == "$(REPOSITORY_ROOT)/target/"* ]] || { \
		printf '%s\n' '[postgresql] error: TARGET_ROOT must be an absolute path below the repository target directory' >&2; exit 1; }; \
	mkdir -p -- "$${target_root}/container-context"; \
	docker build --platform linux/amd64 --tag "$(IMAGE)" \
		--file "$(CONTAINERFILE)" "$${target_root}/container-context"

archives: prepare-source build-image
	@target_root="$$(cd '$(TARGET_ROOT)' && pwd -P)"; \
	rm -rf -- "$${target_root}/build" "$${target_root}/output" "$${target_root}/tmp"; \
	mkdir -m 0700 -- "$${target_root}/build" "$${target_root}/output" "$${target_root}/tmp"; \
	docker run --rm --network=none --platform linux/amd64 \
		--user "$$(id -u):$$(id -g)" \
		--volume "$${target_root}/prepared-source:/build/postgresql-source:ro" \
		--volume "$${target_root}/build:/build/postgresql-build:rw" \
		--volume "$${target_root}/tmp:/build/tmp:rw" \
		--volume "$${target_root}/output:/output:rw" \
		--volume "$(SUPPORT_BUILDER):/build/support_bundle.py:ro" \
		"$(IMAGE)" bash -euo pipefail -c '\
		export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin; \
		cd /build/postgresql-build; \
		env -i PATH="$${PATH}" AR=ar ARFLAGS=rcD CC=gcc \
			CFLAGS="-O2 -g0 -fPIC -ffile-prefix-map=/build=. -fdebug-prefix-map=/build=. -Wdate-time" \
			CONFIG_SITE=/dev/null CPPFLAGS=-I/build/zlib/include LANG=C.UTF-8 LC_ALL=C.UTF-8 \
			LDFLAGS="-L/build/zlib/lib -Wl,--build-id=none" \
			PKG_CONFIG_LIBDIR=/nonexistent RANLIB=ranlib SOURCE_DATE_EPOCH=$(SOURCE_DATE_EPOCH) \
			TMPDIR=/build/tmp TZ=UTC0 /build/postgresql-source/configure $(CONFIGURE_FLAGS); \
		env -i PATH="$${PATH}" AR=ar ARFLAGS=rcD CC=gcc \
			CFLAGS="-O2 -g0 -fPIC -ffile-prefix-map=/build=. -fdebug-prefix-map=/build=. -Wdate-time" \
			CPPFLAGS=-I/build/zlib/include LANG=C.UTF-8 LC_ALL=C.UTF-8 \
			LDFLAGS="-L/build/zlib/lib -Wl,--build-id=none" \
			PKG_CONFIG_LIBDIR=/nonexistent RANLIB=ranlib SOURCE_DATE_EPOCH=$(SOURCE_DATE_EPOCH) \
			TMPDIR=/build/tmp TZ=UTC0 make -j1 -C /build/postgresql-build/src/backend \
			ORNA_EMBEDDED_ZLIB_ARCHIVE=/build/zlib/lib/libz.a \
			orna_postgres18_lifecycle_archives; \
		env -i PATH="$${PATH}" AR=ar ARFLAGS=rcD CC=gcc \
			CFLAGS="-O2 -g0 -fPIC -ffile-prefix-map=/build=. -fdebug-prefix-map=/build=. -Wdate-time" \
			CPPFLAGS=-I/build/zlib/include LANG=C.UTF-8 LC_ALL=C.UTF-8 \
			LDFLAGS="-L/build/zlib/lib -Wl,--build-id=none" \
			PKG_CONFIG_LIBDIR=/nonexistent RANLIB=ranlib SOURCE_DATE_EPOCH=$(SOURCE_DATE_EPOCH) \
			TMPDIR=/build/tmp TZ=UTC0 make -j1 -C /build/postgresql-build/src/timezone zic; \
		mkdir -m 0700 /build/tmp/generated-timezone; \
		umask 077; \
		env -i PATH="$${PATH}" SOURCE_DATE_EPOCH=$(SOURCE_DATE_EPOCH) TZ=UTC0 \
			/build/postgresql-build/src/timezone/zic -d /build/tmp/generated-timezone \
			/build/postgresql-source/src/timezone/data/tzdata.zi; \
		python3 /build/support_bundle.py /build/postgresql-source \
			/build/postgresql-build /build/tmp/generated-timezone \
			/build/tmp/support-work /output; \
		install -m 0644 /build/postgresql-build/src/backend/liborna_postgres18_backend.a /output/; \
		install -m 0644 /build/postgresql-build/src/bin/initdb/liborna_postgres18_initdb.a /output/; \
		install -m 0644 /build/postgresql-build/src/bin/initdb/initdb-redefine-symbols.txt /output/; \
		install -m 0644 /build/postgresql-build/src/bin/initdb/initdb-defined-symbols.txt /output/; \
		install -m 0644 /build/postgresql-build/src/bin/initdb/initdb-undefined-symbols.txt /output/; \
		install -m 0644 /build/postgresql-source/COPYRIGHT /output/POSTGRESQL-LICENSE; \
		nm --format=posix --extern-only --defined-only \
			/build/postgresql-build/src/backend/liborna_postgres18_backend.a | \
			LC_ALL=C sort > /output/backend-defined-symbols.txt; \
		nm --format=posix --extern-only --undefined-only \
			/build/postgresql-build/src/backend/liborna_postgres18_backend.a | \
			LC_ALL=C sort > /output/backend-undefined-symbols.txt; \
		test -z "$$(nm --extern-only --undefined-only \
			/build/postgresql-build/src/backend/main/main.o | \
			awk "\$$NF ~ /^orna_postgres18_/ { print \$$NF }")"; \
		test "$$(ar t /output/liborna_postgres18_backend.a)" = liborna_postgres18_backend.o; \
		test "$$(ar t /output/liborna_postgres18_initdb.a)" = liborna_postgres18_initdb.o; \
		test "$$(nm --extern-only --defined-only /output/liborna_postgres18_backend.a | \
			awk "\$$NF == \"orna_postgres18_entry\" { n++ } END { print n + 0 }")" = 1; \
		test "$$(nm --extern-only --defined-only /output/liborna_postgres18_backend.a | \
			awk "\$$NF == \"main\" { n++ } END { print n + 0 }")" = 0; \
		test "$$(nm --extern-only --defined-only /output/liborna_postgres18_initdb.a | \
			awk "\$$NF == \"orna_postgres18_initdb_entry\" { n++ } END { print n + 0 }")" = 1; \
		test "$$(nm --extern-only --defined-only /output/liborna_postgres18_backend.a | \
			awk "\$$NF == \"deflate\" { n++ } END { print n + 0 }")" = 1; \
		actual_bridges="$$(nm --extern-only --undefined-only \
			/output/liborna_postgres18_initdb.a | \
			awk "\$$NF ~ /^orna_postgres18_/ { print \$$NF }" | LC_ALL=C sort -u | \
			tr "\\n" " ")"; \
		test "$${actual_bridges}" = "orna_postgres18_entry orna_postgres18_install_exec_filter orna_postgres18_set_initialisation_child_capability orna_postgres18_set_system_functions_initialisation_capability orna_postgres18_support_root "; \
		test -z "$$(nm --extern-only --defined-only /output/liborna_postgres18_initdb.a | \
			awk "NF >= 2 && \$$NF !~ /:$$/ && \$$NF != \"orna_postgres18_initdb_entry\" && \$$NF !~ /^orna_postgres18_initdb_/ { print \$$NF }")"; \
		! find /output -type f \( -perm /111 -o -name "*.so" -o -name "*.so.*" \) -print -quit | grep -q .; \
		find /output -type f -exec chmod 0644 {} +'; \
	printf '%s\n' "[postgresql] built embedded archives below $${target_root}/output"

support-data: archives

lifecycle: archives
	@target_root="$$(cd '$(TARGET_ROOT)' && pwd -P)"; \
	docker run --rm --network=none --platform linux/amd64 --user 0:0 \
		--volume "$${target_root}:/proof:rw" \
		--volume "$(LIFECYCLE_PROBE):/input/lifecycle_probe.c:ro" \
		--volume "$(LIFECYCLE_VERIFIER):/input/verify_lifecycle.py:ro" \
		"$(IMAGE)" bash -euo pipefail -c '\
		export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin; \
		if awk -F: '\''$$1 == "orna_kernel" || $$3 == $(LIFECYCLE_UID) { found = 1 } END { exit !found }'\'' /etc/passwd; then \
			printf "%s\n" "[postgresql] error: lifecycle passwd identity collides" >&2; exit 1; \
		fi; \
		if awk -F: '\''$$1 == "orna_kernel" || $$3 == $(LIFECYCLE_GID) { found = 1 } END { exit !found }'\'' /etc/group; then \
			printf "%s\n" "[postgresql] error: lifecycle group identity collides" >&2; exit 1; \
		fi; \
		printf "%s\n" "orna_kernel:x:$(LIFECYCLE_GID):" >> /etc/group; \
		printf "%s\n" "orna_kernel:x:$(LIFECYCLE_UID):$(LIFECYCLE_GID)::/nonexistent:/usr/sbin/nologin" >> /etc/passwd; \
		rm -rf -- /proof/lifecycle; \
		install -d -m 0700 /proof/lifecycle /proof/lifecycle/support \
			/proof/lifecycle/pgdata /proof/lifecycle/socket; \
		umask 077; \
		tar --extract --file=/proof/output/embedded-postgresql-support.tar \
			--directory=/proof/lifecycle/support --no-same-owner --no-same-permissions; \
		test -z "$$(find /proof/lifecycle/support -mindepth 1 ! -type d ! -type f -print -quit)"; \
		find /proof/lifecycle/support -type d -exec chmod 0700 {} +; \
		find /proof/lifecycle/support -type f -exec chmod 0600 {} +; \
		chown -R $(LIFECYCLE_UID):$(LIFECYCLE_GID) /proof/lifecycle; \
		gcc -D_GNU_SOURCE -std=c11 -Wall -Wextra -Werror -Wl,--build-id=none \
			-o /proof/tmp/lifecycle-probe /input/lifecycle_probe.c \
			/proof/output/liborna_postgres18_initdb.a \
			/proof/output/liborna_postgres18_backend.a -lz -lm; \
		env -i PATH="$${PATH}" LANG=C.UTF-8 LC_ALL=C.UTF-8 TZ=UTC0 \
			strace -ff -qq -s 4096 -o /proof/lifecycle/lifecycle.strace \
			-e trace=%process,%file,%memory,%network,seccomp,prctl,write \
			/proof/tmp/lifecycle-probe \
			/proof/lifecycle/support /proof/lifecycle/pgdata /proof/lifecycle/socket \
			$(LIFECYCLE_UID) $(LIFECYCLE_GID) $(LIFECYCLE_PORT) \
			/proof/lifecycle/embedded-initialisation-report.json \
			>/proof/lifecycle/probe.stdout 2>/proof/lifecycle/markers; \
		test ! -s /proof/lifecycle/probe.stdout; \
		printf "%s\n" preload-complete reference-entry-start reference-entry-complete \
			hostile-entry-start hostile-entry-complete initdb-start initdb-complete \
			postmaster-start pgwire-ready query-complete postmaster-sigint complete \
			> /proof/lifecycle/expected-markers; \
		cmp /proof/lifecycle/expected-markers /proof/lifecycle/markers; \
		python3 /input/verify_lifecycle.py /proof/lifecycle/lifecycle.strace \
			/proof/lifecycle/markers \
			/proof/lifecycle/embedded-initialisation-report.json \
			/proof/lifecycle/embedded-initialisation-report.json.stdout \
			/proof/tmp/lifecycle-probe; \
		install -m 0644 /proof/lifecycle/embedded-initialisation-report.json \
			/proof/output/embedded-initialisation-report.json; \
		install -m 0644 /proof/lifecycle/embedded-initialisation-report.json.stdout \
			/proof/output/embedded-initialisation.stdout; \
		chown --reference=/proof/output /proof/tmp/lifecycle-probe; \
		chmod 0500 /proof/tmp/lifecycle-probe; \
		rm -rf -- /proof/lifecycle'; \
	printf '%s\n' "[postgresql] linked lifecycle passed below $${target_root}/output"

verify-lifecycle:
	@target_root='$(TARGET_ROOT)'; \
	[[ "$${target_root}" == "$(REPOSITORY_ROOT)/target/"* ]] || { \
		printf '%s\n' '[postgresql] error: TARGET_ROOT must be an absolute path below the repository target directory' >&2; exit 1; }; \
	mkdir -p -- "$${target_root}"; \
	one="$${target_root}/build-one"; two="$${target_root}/build-two"; \
	$(MAKE) lifecycle TARGET_ROOT="$${one}"; \
	$(MAKE) lifecycle TARGET_ROOT="$${two}"; \
	cmp <(find "$${one}/output" -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort) \
		<(find "$${two}/output" -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort); \
	while IFS= read -r name; do \
		cmp "$${one}/output/$${name}" "$${two}/output/$${name}"; \
	done < <(find "$${one}/output" -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort); \
	cmp "$${one}/tmp/lifecycle-probe" "$${two}/tmp/lifecycle-probe"; \
	printf '%s\n' "[postgresql] linked lifecycle reproduced twice below $${target_root}"

manifest: lifecycle
	@python3 "$(MANIFEST_BUILDER)" "$(REPOSITORY_ROOT)" "$(TARGET_ROOT)" "$(IMAGE)"
	@printf '%s\n' "[postgresql] recorded build evidence below $(TARGET_ROOT)/output"

update-source:
	@reference='$(POSTGRESQL_REF)'; \
	[[ "$${reference}" =~ ^[0-9a-f]{40}$$ ]] || { \
		printf '%s\n' '[postgresql] error: POSTGRESQL_REF must be one full lowercase commit identifier' >&2; exit 1; }; \
	[[ -z "$$(git -C "$(UPSTREAM_SOURCE)" status --porcelain=v1 --untracked-files=all)" ]] || { \
		printf '%s\n' '[postgresql] error: the upstream checkout is not clean' >&2; exit 1; }; \
	[[ "$$(git -C "$(UPSTREAM_SOURCE)" remote get-url origin)" == 'https://github.com/postgres/postgres.git' ]] || { \
		printf '%s\n' '[postgresql] error: the upstream origin is not accepted' >&2; exit 1; }; \
	git -C "$(UPSTREAM_SOURCE)" fetch --no-tags origin "$${reference}"; \
	[[ "$$(git -C "$(UPSTREAM_SOURCE)" cat-file -t "$${reference}")" == commit ]] || exit 1; \
	git -C "$(UPSTREAM_SOURCE)" checkout --detach "$${reference}"; \
	printf '%s\n' '[postgresql] source updated; review and commit the superproject gitlink change'
