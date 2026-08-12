/*-------------------------------------------------------------------------
 *
 * orna_embedded.c
 *    Private process-local state for the embedded PostgreSQL 18 engine.
 *
 *-------------------------------------------------------------------------
 */
#include "postgres.h"

#include <errno.h>
#include <fcntl.h>
#include <stddef.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#ifdef __linux__
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <sys/prctl.h>
#include <sys/shm.h>
#include <sys/syscall.h>
#include <sys/mman.h>

#ifndef __X32_SYSCALL_BIT
#error "the embedded PostgreSQL seccomp filter requires x86-64 syscall definitions"
#endif
#endif

#include "catalog/catversion.h"
#include "catalog/pg_control.h"
#include "miscadmin.h"
#include "orna_embedded.h"
#include "port/pg_crc32c.h"

static char orna_support_root[MAXPGPATH];
static bool orna_support_root_is_set = false;
static bool orna_system_functions_initialisation_capability = false;
static bool orna_initialisation_child_capability = false;

int
orna_postgres18_set_support_root(const char *absolute_root)
{
	if (absolute_root == NULL || absolute_root[0] != '/' ||
		strlen(absolute_root) >= sizeof(orna_support_root))
		return -1;
	if (orna_support_root_is_set)
		return strcmp(absolute_root, orna_support_root) == 0 ? 0 : -1;

	strlcpy(orna_support_root, absolute_root, sizeof(orna_support_root));
	orna_support_root_is_set = true;
	return 0;
}

const char *
orna_postgres18_support_root(void)
{
	return orna_support_root_is_set ? orna_support_root : NULL;
}

void
orna_postgres18_set_system_functions_initialisation_capability(bool enabled)
{
	orna_system_functions_initialisation_capability = enabled;
}

bool
orna_postgres18_has_system_functions_initialisation_capability(void)
{
	return orna_system_functions_initialisation_capability;
}

void
orna_postgres18_set_initialisation_child_capability(bool enabled)
{
	orna_initialisation_child_capability = enabled;
}

bool
orna_postgres18_has_initialisation_child_capability(void)
{
	return orna_initialisation_child_capability;
}

int
orna_postgres18_read_control(const char *data_directory,
							 OrnaPostgres18ControlData *control)
{
	char		path[MAXPGPATH];
	ControlFileData control_file;
	struct stat metadata;
	pg_crc32c	crc;
	ssize_t		read_count;
	size_t		offset = 0;
	int			descriptor;
	int			written;

	if (data_directory == NULL || data_directory[0] != '/' ||
		strlen(data_directory) >= MAXPGPATH || control == NULL)
		return -1;
	written = snprintf(path, sizeof(path), "%s/global/pg_control", data_directory);
	if (written < 0 || (size_t) written >= sizeof(path))
		return -1;
	descriptor = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW | PG_BINARY, 0);
	if (descriptor < 0)
		return -1;
	if (fstat(descriptor, &metadata) != 0 || !S_ISREG(metadata.st_mode) ||
		metadata.st_nlink != 1 || metadata.st_size != PG_CONTROL_FILE_SIZE)
	{
		(void) close(descriptor);
		return -1;
	}
	while (offset < sizeof(control_file))
	{
		read_count = read(descriptor, ((char *) &control_file) + offset,
						  sizeof(control_file) - offset);
		if (read_count < 0 && errno == EINTR)
			continue;
		if (read_count <= 0)
		{
			(void) close(descriptor);
			return -1;
		}
		offset += read_count;
	}
	if (close(descriptor) != 0)
		return -1;

	INIT_CRC32C(crc);
	COMP_CRC32C(crc, &control_file, offsetof(ControlFileData, crc));
	FIN_CRC32C(crc);
	if (!EQ_CRC32C(crc, control_file.crc) ||
		control_file.pg_control_version != PG_CONTROL_VERSION ||
		control_file.catalog_version_no != CATALOG_VERSION_NO)
		return -1;

	control->system_identifier = control_file.system_identifier;
	control->pg_control_version = control_file.pg_control_version;
	control->catalog_version = control_file.catalog_version_no;
	control->state = control_file.state;
	control->data_checksum_version = control_file.data_checksum_version;
	return 0;
}

int
orna_postgres18_install_exec_filter(void)
{
#ifdef __linux__
	struct sock_filter filter[] = {
		BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
				 offsetof(struct seccomp_data, arch)),
		BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
		BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
		BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
				 offsetof(struct seccomp_data, nr)),
		BPF_JUMP(BPF_JMP | BPF_JSET | BPF_K, __X32_SYSCALL_BIT, 0, 1),
		BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EPERM),
		BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_execve, 0, 1),
		BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EPERM),
		BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_execveat, 0, 1),
		BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EPERM),
		BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_memfd_create, 0, 1),
		BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EPERM),
		BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_mmap, 0, 4),
		BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
				 offsetof(struct seccomp_data, args[2])),
		BPF_JUMP(BPF_JMP | BPF_JSET | BPF_K, PROT_EXEC, 0, 1),
		BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EPERM),
		BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
				 offsetof(struct seccomp_data, nr)),
		BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_mprotect, 0, 4),
		BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
				 offsetof(struct seccomp_data, args[2])),
		BPF_JUMP(BPF_JMP | BPF_JSET | BPF_K, PROT_EXEC, 0, 1),
		BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EPERM),
		BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
				 offsetof(struct seccomp_data, nr)),
#ifdef __NR_pkey_mprotect
		BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_pkey_mprotect, 0, 4),
		BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
				 offsetof(struct seccomp_data, args[2])),
		BPF_JUMP(BPF_JMP | BPF_JSET | BPF_K, PROT_EXEC, 0, 1),
		BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EPERM),
		BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
				 offsetof(struct seccomp_data, nr)),
#endif
		BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_shmat, 0, 3),
		BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
				 offsetof(struct seccomp_data, args[2])),
		BPF_JUMP(BPF_JMP | BPF_JSET | BPF_K, SHM_EXEC, 0, 1),
		BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EPERM),
		BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
	};
	struct sock_fprog program = {
		.len = lengthof(filter),
		.filter = filter,
	};

	if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0)
		return -1;
	if (syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &program) != 0)
		return -1;
	return 0;
#else
	errno = ENOSYS;
	return -1;
#endif
}
