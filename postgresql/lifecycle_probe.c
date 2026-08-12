#include <arpa/inet.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <limits.h>
#include <netinet/in.h>
#include <pwd.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/types.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

extern int orna_postgres18_entry(int argc, char *argv[]);
extern int orna_postgres18_initdb_entry(const char *data_directory);
extern int orna_postgres18_set_support_root(const char *absolute_root);

#define ARRAY_LENGTH(array) (sizeof(array) / sizeof((array)[0]))
#define MAX_FRAME_SIZE (1024U * 1024U)
#define READINESS_ATTEMPTS 600
#define WAIT_INTERVAL_NS 50000000L
#define FAST_STOP_ATTEMPTS 1200
#define IMMEDIATE_STOP_ATTEMPTS 600
#define EXPECTED_SUPPORT_FILES 620

static pid_t live_postmaster = -1;

enum wait_result
{
	WAIT_RESULT_RUNNING,
	WAIT_RESULT_CLEAN,
	WAIT_RESULT_FAILED,
};

static void
wait_interval(void)
{
	struct timespec delay = {0, WAIT_INTERVAL_NS};

	while (nanosleep(&delay, &delay) != 0 && errno == EINTR)
		;
}

static void
stop_live_postmaster(void)
{
	int status;
	pid_t waited;

	if (live_postmaster <= 0)
		return;
	(void) kill(live_postmaster, SIGQUIT);
	do
	{
		waited = waitpid(live_postmaster, &status, 0);
	} while (waited < 0 && errno == EINTR);
	live_postmaster = -1;
}

static void
fail(const char *message)
{
	int saved_errno = errno;

	stop_live_postmaster();
	if (saved_errno != 0)
		fprintf(stderr, "lifecycle probe failed: %s: %s\n", message, strerror(saved_errno));
	else
		fprintf(stderr, "lifecycle probe failed: %s\n", message);
	exit(1);
}

static void
marker(const char *message)
{
	size_t length = strlen(message);

	if (write(STDERR_FILENO, message, length) != (ssize_t) length ||
		write(STDERR_FILENO, "\n", 1) != 1)
		fail("could not write a lifecycle marker");
}

static unsigned long
parse_number(const char *text, const char *name, unsigned long maximum)
{
	char *end = NULL;
	unsigned long value;

	errno = 0;
	value = strtoul(text, &end, 10);
	if (errno != 0 || end == text || *end != '\0' || value == 0 || value > maximum)
	{
		errno = 0;
		fprintf(stderr, "lifecycle probe failed: %s is not accepted\n", name);
		exit(1);
	}
	return value;
}

static void
require_absolute_path(const char *path, const char *name)
{
	if (path == NULL || path[0] != '/' || strlen(path) >= PATH_MAX || strstr(path, "/../") != NULL)
	{
		fprintf(stderr, "lifecycle probe failed: %s is not an accepted absolute path\n", name);
		exit(1);
	}
}

static void
join_path(char *destination, size_t size, const char *left, const char *right)
{
	int written = snprintf(destination, size, "%s%s", left, right);

	if (written < 0 || (size_t) written >= size)
	{
		errno = 0;
		fail("a lifecycle path is too long");
	}
}

static void
require_directory(const char *path, uid_t uid, gid_t gid, bool empty)
{
	struct stat path_stat;
	DIR *directory;
	struct dirent *entry;

	if (lstat(path, &path_stat) != 0)
		fail("could not inspect a lifecycle directory");
	if (!S_ISDIR(path_stat.st_mode) || (path_stat.st_mode & 07777) != 0700 ||
		path_stat.st_uid != uid || path_stat.st_gid != gid)
	{
		errno = 0;
		fail("a lifecycle directory has unexpected metadata");
	}
	if (!empty)
		return;
	directory = opendir(path);
	if (directory == NULL)
		fail("could not open a lifecycle directory");
	errno = 0;
	while ((entry = readdir(directory)) != NULL)
	{
		if (strcmp(entry->d_name, ".") != 0 && strcmp(entry->d_name, "..") != 0)
		{
			(void) closedir(directory);
			errno = 0;
			fail("a lifecycle directory is not empty");
		}
	}
	if (errno != 0 || closedir(directory) != 0)
		fail("could not finish inspecting a lifecycle directory");
}

static size_t
verify_support_directory(const char *path, uid_t uid, gid_t gid)
{
	struct stat path_stat;
	DIR *directory;
	struct dirent *entry;
	size_t files = 0;

	if (lstat(path, &path_stat) != 0)
		fail("could not inspect the support root");
	if (!S_ISDIR(path_stat.st_mode) || (path_stat.st_mode & 07777) != 0700 ||
		path_stat.st_uid != uid || path_stat.st_gid != gid)
	{
		errno = 0;
		fail("support directory metadata is not accepted");
	}
	directory = opendir(path);
	if (directory == NULL)
		fail("could not open the support root");
	errno = 0;
	while ((entry = readdir(directory)) != NULL)
	{
		char child[PATH_MAX];

		if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0)
			continue;
		if (strchr(entry->d_name, '/') != NULL)
		{
			(void) closedir(directory);
			errno = 0;
			fail("support member name is not accepted");
		}
		if (snprintf(child, sizeof(child), "%s/%s", path, entry->d_name) >= (int) sizeof(child))
		{
			(void) closedir(directory);
			errno = 0;
			fail("support member path is too long");
		}
		if (lstat(child, &path_stat) != 0)
		{
			(void) closedir(directory);
			fail("could not inspect a support member");
		}
		if (S_ISDIR(path_stat.st_mode))
			files += verify_support_directory(child, uid, gid);
		else if (S_ISREG(path_stat.st_mode) && (path_stat.st_mode & 07777) == 0600 &&
				 path_stat.st_nlink == 1 && path_stat.st_uid == uid && path_stat.st_gid == gid)
			files++;
		else
		{
			(void) closedir(directory);
			errno = 0;
			fail("support member metadata is not accepted");
		}
	}
	if (errno != 0 || closedir(directory) != 0)
		fail("could not finish inspecting the support tree");
	return files;
}

static size_t
count_threads(void)
{
	DIR *directory = opendir("/proc/self/task");
	struct dirent *entry;
	size_t count = 0;

	if (directory == NULL)
		fail("could not inspect the lifecycle probe thread count");
	errno = 0;
	while ((entry = readdir(directory)) != NULL)
	{
		if (entry->d_name[0] >= '0' && entry->d_name[0] <= '9')
			count++;
	}
	if (errno != 0 || closedir(directory) != 0)
		fail("could not finish inspecting the lifecycle probe thread count");
	return count;
}

static void
verify_identity(const char *name, uid_t uid, gid_t gid)
{
	struct passwd *password = getpwnam(name);
	struct group *group = getgrnam(name);

	if (password == NULL || group == NULL || password->pw_uid != uid ||
		password->pw_gid != gid || group->gr_gid != gid || strcmp(password->pw_name, name) != 0 ||
		strcmp(group->gr_name, name) != 0)
	{
		errno = 0;
		fail("lifecycle identity does not resolve exactly");
	}
}

static void
drop_identity(uid_t uid, gid_t gid)
{
	int group_count;

	if (setgroups(0, NULL) != 0 || setresgid(gid, gid, gid) != 0 || setresuid(uid, uid, uid) != 0)
		fail("could not drop lifecycle probe credentials");
	group_count = getgroups(0, NULL);
	if (getuid() != uid || geteuid() != uid || getgid() != gid || getegid() != gid || group_count != 0)
	{
		errno = 0;
		fail("lifecycle probe credentials are not exact after the drop");
	}
	errno = 0;
	if (setuid(0) != -1 || errno != EPERM)
	{
		errno = 0;
		fail("lifecycle probe could regain root credentials");
	}
	if (prctl(PR_SET_DUMPABLE, 1, 0, 0, 0) != 0)
		fail("could not make lifecycle child identity inspectable");
}

static int
open_capture(const char *path)
{
	int descriptor = open(path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0600);

	if (descriptor < 0 || fchmod(descriptor, 0600) != 0)
		fail("could not create a lifecycle capture");
	return descriptor;
}

static void
reset_child_signals(void)
{
	struct sigaction action;
	sigset_t mask;
	int signal_number;

	memset(&action, 0, sizeof(action));
	action.sa_handler = SIG_DFL;
	sigemptyset(&action.sa_mask);
	for (signal_number = 1; signal_number < NSIG; signal_number++)
	{
		if (signal_number != SIGKILL && signal_number != SIGSTOP)
			(void) sigaction(signal_number, &action, NULL);
	}
	sigemptyset(&mask);
	if (sigprocmask(SIG_SETMASK, &mask, NULL) != 0)
		_exit(126);
}

static int
wait_exact(pid_t child)
{
	int status;
	pid_t waited;

	do
	{
		waited = waitpid(child, &status, 0);
	} while (waited < 0 && errno == EINTR);
	if (waited != child || !WIFEXITED(status))
		return -1;
	return WEXITSTATUS(status);
}

static void
redirect_child(int standard_output, int standard_error)
{
	if (dup2(standard_output, STDOUT_FILENO) < 0 || dup2(standard_error, STDERR_FILENO) < 0)
		_exit(126);
	if (standard_output != STDOUT_FILENO)
		(void) close(standard_output);
	if (standard_error != STDERR_FILENO && standard_error != standard_output)
		(void) close(standard_error);
}

static void prepare_writable_arguments(char *storage, size_t storage_size, char **arguments,
									   const char *const *values, size_t value_count);

static void
run_describe(const char *support_root, const char *argv0, const char *stdout_path,
			 const char *stderr_path)
{
	const char *argument_values[] = {argv0, "--describe-config"};
	char argument_storage[PATH_MAX + 32];
	char *arguments[ARRAY_LENGTH(argument_values) + 1];
	int standard_output = open_capture(stdout_path);
	int standard_error = open_capture(stderr_path);
	pid_t child;
	int status;

	prepare_writable_arguments(argument_storage, sizeof(argument_storage), arguments,
							   argument_values, ARRAY_LENGTH(argument_values));
	if (count_threads() != 1)
	{
		errno = 0;
		fail("lifecycle probe is not single-threaded before a linked entry fork");
	}
	if (fflush(NULL) != 0)
		fail("could not flush before a linked entry fork");
	child = fork();
	if (child < 0)
		fail("could not fork a describe-config role");
	if (child == 0)
	{
		reset_child_signals();
		redirect_child(standard_output, standard_error);
		if (orna_postgres18_set_support_root(support_root) != 0)
			_exit(124);
		_exit(orna_postgres18_entry(2, arguments));
	}
	if (close(standard_output) != 0 || close(standard_error) != 0)
		fail("could not close a describe-config capture");
	status = wait_exact(child);
	if (status != 0)
	{
		errno = 0;
		fail("linked describe-config role failed");
	}
}

static void
require_empty_file(const char *path)
{
	struct stat path_stat;

	if (lstat(path, &path_stat) != 0)
		fail("could not inspect an empty capture");
	if (!S_ISREG(path_stat.st_mode) || (path_stat.st_mode & 07777) != 0600 ||
		path_stat.st_nlink != 1 || path_stat.st_size != 0)
	{
		errno = 0;
		fail("a lifecycle error capture is not empty and private");
	}
}

static void
require_equal_files(const char *left, const char *right)
{
	int left_descriptor = open(left, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
	int right_descriptor = open(right, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
	struct stat left_stat;
	struct stat right_stat;
	unsigned char left_buffer[65536];
	unsigned char right_buffer[65536];
	ssize_t left_read;
	ssize_t right_read;

	if (left_descriptor < 0 || right_descriptor < 0 || fstat(left_descriptor, &left_stat) != 0 ||
		fstat(right_descriptor, &right_stat) != 0)
		fail("could not inspect describe-config captures");
	if (left_stat.st_size <= 0 || left_stat.st_size != right_stat.st_size)
	{
		errno = 0;
		fail("describe-config captures do not have the same non-empty length");
	}
	do
	{
		do
		{
			left_read = read(left_descriptor, left_buffer, sizeof(left_buffer));
		} while (left_read < 0 && errno == EINTR);
		do
		{
			right_read = read(right_descriptor, right_buffer, sizeof(right_buffer));
		} while (right_read < 0 && errno == EINTR);
		if (left_read < 0 || right_read != left_read ||
			(left_read > 0 && memcmp(left_buffer, right_buffer, (size_t) left_read) != 0))
		{
			errno = 0;
			fail("hostile argv0 changed linked PostgreSQL output");
		}
	} while (left_read > 0);
	if (close(left_descriptor) != 0 || close(right_descriptor) != 0)
		fail("could not close describe-config captures");
}

static void
run_initdb(const char *support_root, const char *data_root, const char *log_path)
{
	int log_descriptor = open_capture(log_path);
	pid_t child;
	int status;

	if (count_threads() != 1)
	{
		errno = 0;
		fail("lifecycle probe is not single-threaded before initialisation");
	}
	if (fflush(NULL) != 0)
		fail("could not flush before initialisation");
	child = fork();
	if (child < 0)
		fail("could not fork the linked initialiser");
	if (child == 0)
	{
		reset_child_signals();
		redirect_child(log_descriptor, log_descriptor);
		if (orna_postgres18_set_support_root(support_root) != 0)
			_exit(124);
		_exit(orna_postgres18_initdb_entry(data_root));
	}
	if (close(log_descriptor) != 0)
		fail("could not close the initialiser log");
	status = wait_exact(child);
	if (status != 0)
	{
		errno = 0;
		fail("linked initialisation failed");
	}
}

static void
prepare_writable_arguments(char *storage, size_t storage_size, char **arguments,
						   const char *const *values, size_t value_count)
{
	char *cursor = storage;
	size_t remaining = storage_size;

	for (size_t index = 0; index < value_count; index++)
	{
		size_t length = strlen(values[index]) + 1;

		if (length > remaining)
		{
			errno = 0;
			fail("postmaster arguments exceed the writable argument buffer");
		}
		arguments[index] = cursor;
		memcpy(cursor, values[index], length);
		cursor += length;
		remaining -= length;
	}
	arguments[value_count] = NULL;
}

static pid_t
start_postmaster(const char *support_root, const char *data_root, const char *socket_root,
				 const char *port, const char *log_path)
{
	const char *argument_values[] = {
		"/usr/bin/orna", "-D", data_root, "-k", socket_root, "-h", "", "-p", port,
	};
	char argument_storage[(2 * PATH_MAX) + 64];
	char *arguments[ARRAY_LENGTH(argument_values) + 1];
	int log_descriptor;
	pid_t child;

	prepare_writable_arguments(argument_storage, sizeof(argument_storage), arguments,
						   argument_values, ARRAY_LENGTH(argument_values));
	log_descriptor = open_capture(log_path);
	if (count_threads() != 1)
	{
		errno = 0;
		fail("lifecycle probe is not single-threaded before postmaster start");
	}
	if (fflush(NULL) != 0)
		fail("could not flush before postmaster start");
	child = fork();
	if (child < 0)
		fail("could not fork the linked postmaster");
	if (child == 0)
	{
		reset_child_signals();
		redirect_child(log_descriptor, log_descriptor);
		if (orna_postgres18_set_support_root(support_root) != 0)
			_exit(124);
		_exit(orna_postgres18_entry(9, arguments));
	}
	if (close(log_descriptor) != 0)
		fail("could not close the postmaster log");
	return child;
}

static void
write_all(int descriptor, const void *buffer, size_t length)
{
	const unsigned char *cursor = buffer;

	while (length > 0)
	{
		ssize_t written = write(descriptor, cursor, length);

		if (written < 0 && errno == EINTR)
			continue;
		if (written <= 0)
			fail("PostgreSQL protocol write failed");
		cursor += written;
		length -= (size_t) written;
	}
}

static void
read_all(int descriptor, void *buffer, size_t length)
{
	unsigned char *cursor = buffer;

	while (length > 0)
	{
		ssize_t received = read(descriptor, cursor, length);

		if (received < 0 && errno == EINTR)
			continue;
		if (received <= 0)
			fail("PostgreSQL protocol read failed");
		cursor += received;
		length -= (size_t) received;
	}
}

static uint16_t
read_u16(const unsigned char *bytes)
{
	uint16_t value;

	memcpy(&value, bytes, sizeof(value));
	return ntohs(value);
}

static uint32_t
read_u32(const unsigned char *bytes)
{
	uint32_t value;

	memcpy(&value, bytes, sizeof(value));
	return ntohl(value);
}

static size_t
receive_frame(int descriptor, char *type, unsigned char *payload, size_t capacity)
{
	unsigned char length_bytes[4];
	uint32_t frame_length;

	read_all(descriptor, type, 1);
	read_all(descriptor, length_bytes, sizeof(length_bytes));
	frame_length = read_u32(length_bytes);
	if (frame_length < 4 || frame_length - 4 > capacity || frame_length > MAX_FRAME_SIZE)
	{
		errno = 0;
		fail("PostgreSQL protocol frame length is not accepted");
	}
	read_all(descriptor, payload, frame_length - 4);
	return frame_length - 4;
}

static void
send_startup(int descriptor)
{
	static const char parameters[] = "user\0orna_kernel\0database\0postgres\0\0";
	uint32_t length = htonl((uint32_t) (8 + sizeof(parameters) - 1));
	uint32_t protocol = htonl(196608U);

	write_all(descriptor, &length, sizeof(length));
	write_all(descriptor, &protocol, sizeof(protocol));
	write_all(descriptor, parameters, sizeof(parameters) - 1);
}

static bool
error_has_sqlstate(const unsigned char *payload, size_t length, const char *expected)
{
	size_t offset = 0;
	bool matched = false;

	while (offset < length && payload[offset] != '\0')
	{
		unsigned char field = payload[offset++];
		const unsigned char *terminator = memchr(payload + offset, '\0', length - offset);

		if (terminator == NULL)
			return false;
		if (field == 'C' && strcmp((const char *) payload + offset, expected) == 0)
			matched = true;
		offset = (size_t) (terminator - payload) + 1;
	}
	return offset < length && payload[offset] == '\0' && matched;
}

static pid_t
receive_startup(int descriptor)
{
	unsigned char payload[MAX_FRAME_SIZE];
	bool authenticated = false;
	bool backend_key = false;
	pid_t backend_pid = -1;

	for (;;)
	{
		char type;
		size_t length = receive_frame(descriptor, &type, payload, sizeof(payload));

		if (type == 'R' && length == 4 && read_u32(payload) == 0 && !authenticated)
			authenticated = true;
		else if (type == 'S' && authenticated && length >= 3 &&
				 memchr(payload, '\0', length) != NULL)
			;
		else if (type == 'K' && authenticated && length == 8 && !backend_key)
		{
			backend_pid = (pid_t) read_u32(payload);
			backend_key = true;
		}
		else if (type == 'Z' && authenticated && backend_key && length == 1 && payload[0] == 'I')
			return backend_pid;
		else if (type == 'E' && !authenticated && error_has_sqlstate(payload, length, "57P03"))
			return -1;
		else
		{
			errno = 0;
			fail("PostgreSQL startup response is not accepted");
		}
	}
}

static int
connect_postmaster(const char *socket_root, unsigned long port, pid_t postmaster, pid_t *backend_pid)
{
	char socket_path[sizeof(((struct sockaddr_un *) 0)->sun_path)];
	struct sockaddr_un address;
	int attempt;

	if (snprintf(socket_path, sizeof(socket_path), "%s/.s.PGSQL.%lu", socket_root, port) >=
		(int) sizeof(socket_path))
	{
		errno = 0;
		fail("PostgreSQL socket path is too long");
	}
	for (attempt = 0; attempt < READINESS_ATTEMPTS; attempt++)
	{
		int descriptor;
		int status;
		struct timeval timeout = {.tv_sec = 1, .tv_usec = 0};
		pid_t waited = waitpid(postmaster, &status, WNOHANG);

		if (waited == postmaster)
		{
			live_postmaster = -1;
			errno = 0;
			fail("postmaster exited before readiness");
		}
		if (waited < 0)
			fail("could not inspect postmaster readiness");
		descriptor = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
		if (descriptor < 0)
			fail("could not create the private PostgreSQL socket");
		if (setsockopt(descriptor, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout)) != 0 ||
			setsockopt(descriptor, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout)) != 0)
		{
			(void) close(descriptor);
			fail("could not bound the private PostgreSQL socket");
		}
		memset(&address, 0, sizeof(address));
		address.sun_family = AF_UNIX;
		strcpy(address.sun_path, socket_path);
		if (connect(descriptor, (struct sockaddr *) &address, sizeof(address)) == 0)
		{
			pid_t received_backend;

			send_startup(descriptor);
			received_backend = receive_startup(descriptor);
			if (received_backend > 0)
			{
				*backend_pid = received_backend;
				return descriptor;
			}
			if (close(descriptor) != 0)
				fail("could not close a PostgreSQL readiness connection");
			wait_interval();
			continue;
		}
		if (errno != ENOENT && errno != ECONNREFUSED)
		{
			(void) close(descriptor);
			fail("private PostgreSQL readiness connection failed");
		}
		(void) close(descriptor);
		wait_interval();
	}
	errno = 0;
	fail("postmaster did not become ready");
	return -1;
}

static void
send_query(int descriptor, const char *query)
{
	uint32_t length = htonl((uint32_t) (5 + strlen(query)));
	char type = 'Q';

	write_all(descriptor, &type, 1);
	write_all(descriptor, &length, sizeof(length));
	write_all(descriptor, query, strlen(query) + 1);
}

static void
expect_create_database(int descriptor)
{
	unsigned char payload[MAX_FRAME_SIZE];
	char type;
	size_t length;

	length = receive_frame(descriptor, &type, payload, sizeof(payload));
	if (type != 'C' || length != sizeof("CREATE DATABASE") ||
		memcmp(payload, "CREATE DATABASE", sizeof("CREATE DATABASE")) != 0)
	{
		errno = 0;
		fail("CREATE DATABASE response is not accepted");
	}
	length = receive_frame(descriptor, &type, payload, sizeof(payload));
	if (type != 'Z' || length != 1 || payload[0] != 'I')
	{
		errno = 0;
		fail("CREATE DATABASE ready response is not accepted");
	}
}

static void
expect_boolean_assertion(int descriptor)
{
	unsigned char payload[MAX_FRAME_SIZE];
	char type;
	size_t length;
	size_t name_length;

	length = receive_frame(descriptor, &type, payload, sizeof(payload));
	name_length = strlen("accepted") + 1;
	if (type != 'T' || length != 2 + name_length + 18 || read_u16(payload) != 1 ||
		memcmp(payload + 2, "accepted", name_length) != 0 ||
		read_u32(payload + 2 + name_length + 6) != 16 ||
		read_u16(payload + 2 + name_length + 10) != 1 ||
		read_u16(payload + 2 + name_length + 16) != 0)
	{
		errno = 0;
		fail("assertion RowDescription is not accepted");
	}
	length = receive_frame(descriptor, &type, payload, sizeof(payload));
	if (type != 'D' || length != 7 || read_u16(payload) != 1 || read_u32(payload + 2) != 1 ||
		payload[6] != 't')
	{
		errno = 0;
		fail("assertion DataRow is not true");
	}
	length = receive_frame(descriptor, &type, payload, sizeof(payload));
	if (type != 'C' || length != sizeof("SELECT 1") ||
		memcmp(payload, "SELECT 1", sizeof("SELECT 1")) != 0)
	{
		errno = 0;
		fail("assertion CommandComplete is not accepted");
	}
	length = receive_frame(descriptor, &type, payload, sizeof(payload));
	if (type != 'Z' || length != 1 || payload[0] != 'I')
	{
		errno = 0;
		fail("assertion ready response is not accepted");
	}
}

static bool
same_executable(pid_t process, const struct stat *expected)
{
	char path[64];
	struct stat actual;

	if (snprintf(path, sizeof(path), "/proc/%ld/exe", (long) process) >= (int) sizeof(path) ||
		stat(path, &actual) != 0)
		return false;
	return actual.st_dev == expected->st_dev && actual.st_ino == expected->st_ino;
}

static bool
read_parent_pid(pid_t process, pid_t *parent)
{
	char path[64];
	char buffer[4096];
	char *closing;
	FILE *file;
	long value;

	if (snprintf(path, sizeof(path), "/proc/%ld/stat", (long) process) >= (int) sizeof(path))
		return false;
	file = fopen(path, "r");
	if (file == NULL)
		return false;
	if (fgets(buffer, sizeof(buffer), file) == NULL || fclose(file) != 0)
		return false;
	closing = strrchr(buffer, ')');
	if (closing == NULL || sscanf(closing + 1, " %*c %ld", &value) != 1 || value <= 0)
		return false;
	*parent = (pid_t) value;
	return true;
}

static bool
is_descendant(pid_t process, pid_t ancestor)
{
	int depth;

	for (depth = 0; depth < 64 && process > 1; depth++)
	{
		pid_t parent;

		if (!read_parent_pid(process, &parent))
			return false;
		if (parent == ancestor)
			return true;
		if (parent == process)
			return false;
		process = parent;
	}
	return false;
}

static void
verify_process_identity(pid_t postmaster, pid_t backend)
{
	struct stat expected;
	DIR *directory;
	struct dirent *entry;
	size_t descendants = 0;

	if (stat("/proc/self/exe", &expected) != 0)
		fail("could not inspect lifecycle probe executable identity");
	if (!same_executable(postmaster, &expected) || !same_executable(backend, &expected))
	{
		errno = 0;
		fail("postmaster or connected backend does not use the lifecycle probe ELF");
	}
	directory = opendir("/proc");
	if (directory == NULL)
		fail("could not inspect live PostgreSQL roles");
	errno = 0;
	while ((entry = readdir(directory)) != NULL)
	{
		char *end;
		long value;

		if (entry->d_name[0] < '1' || entry->d_name[0] > '9')
			continue;
		value = strtol(entry->d_name, &end, 10);
		if (*end != '\0' || value <= 0 || value > INT_MAX || (pid_t) value == postmaster)
			continue;
		if (is_descendant((pid_t) value, postmaster))
		{
			descendants++;
			if (!same_executable((pid_t) value, &expected))
			{
				(void) closedir(directory);
				errno = 0;
				fail("a live PostgreSQL role does not use the lifecycle probe ELF");
			}
		}
	}
	if (errno != 0 || closedir(directory) != 0)
		fail("could not finish inspecting live PostgreSQL roles");
	if (descendants < 2)
	{
		errno = 0;
		fail("postmaster descendant closure is unexpectedly small");
	}
}

static enum wait_result
wait_for_stop(pid_t process, int attempts)
{
	int attempt;

	for (attempt = 0; attempt < attempts; attempt++)
	{
		int status;
		pid_t waited = waitpid(process, &status, WNOHANG);

		if (waited == process)
		{
			live_postmaster = -1;
			return WIFEXITED(status) && WEXITSTATUS(status) == 0 ?
				WAIT_RESULT_CLEAN : WAIT_RESULT_FAILED;
		}
		if (waited < 0)
			fail("could not wait for postmaster shutdown");
		wait_interval();
	}
	return WAIT_RESULT_RUNNING;
}

static void
write_report(const char *path, bool escalated)
{
	static const char report_without_escalation[] =
		"{\n"
		"  \"cluster_assertions\": true,\n"
		"  \"credential_drop\": true,\n"
		"  \"format\": 1,\n"
		"  \"hostile_authority_rejected\": true,\n"
		"  \"one_executable\": true,\n"
		"  \"postmaster_clean_stop\": true,\n"
		"  \"postmaster_sigquit_escalation\": false,\n"
		"  \"support_members\": 620\n"
		"}\n";
	static const char report_with_escalation[] =
		"{\n"
		"  \"cluster_assertions\": true,\n"
		"  \"credential_drop\": true,\n"
		"  \"format\": 1,\n"
		"  \"hostile_authority_rejected\": true,\n"
		"  \"one_executable\": true,\n"
		"  \"postmaster_clean_stop\": true,\n"
		"  \"postmaster_sigquit_escalation\": true,\n"
		"  \"support_members\": 620\n"
		"}\n";
	const char *content = escalated ? report_with_escalation : report_without_escalation;
	int descriptor = open(path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0600);

	if (descriptor < 0)
		fail("could not create the lifecycle report");
	write_all(descriptor, content, strlen(content));
	if (fsync(descriptor) != 0 || fchmod(descriptor, 0600) != 0 || close(descriptor) != 0)
		fail("could not finish the lifecycle report");
}

static const char assertion_query[] =
	"SELECT ("
	"current_user = 'orna_kernel' AND current_database() = 'postgres' "
	"AND current_setting('server_version_num') = '180004' "
	"AND current_setting('data_checksums') = 'on' "
	"AND current_setting('default_text_search_config') = 'pg_catalog.simple' "
	"AND (SELECT data_page_checksum_version FROM pg_control_init()) = 1 "
	"AND (SELECT count(*) FROM pg_database) = 4 "
	"AND (SELECT count(*) FROM pg_database WHERE datname IN ('orna','postgres','template0','template1')) = 4 "
	"AND (SELECT count(*) FROM pg_database WHERE datname='orna' AND NOT datistemplate AND datallowconn AND datdba=10) = 1 "
	"AND (SELECT count(*) FROM pg_database WHERE datname='postgres' AND NOT datistemplate AND datallowconn AND datdba=10) = 1 "
	"AND (SELECT count(*) FROM pg_database WHERE datname='template0' AND datistemplate AND NOT datallowconn AND datdba=10) = 1 "
	"AND (SELECT count(*) FROM pg_database WHERE datname='template1' AND datistemplate AND datallowconn AND datdba=10) = 1 "
	"AND (SELECT count(*) FROM pg_roles WHERE rolsuper) = 1 "
	"AND (SELECT count(*) FROM pg_roles WHERE rolname='orna_kernel' AND rolsuper) = 1 "
	"AND (SELECT datcollate='C' AND datctype='C' AND datlocale='PG_UNICODE_FAST' AND datlocprovider='b' FROM pg_database WHERE datname=current_database()) "
	"AND (SELECT count(*) FROM pg_language WHERE lanname='plpgsql') = 0 "
	"AND (SELECT count(*) FROM pg_ts_config) = 1 "
	"AND (SELECT count(*) FROM pg_ts_config WHERE cfgname='simple' AND cfgnamespace=11 AND cfgowner=10) = 1 "
	"AND (SELECT count(*) FROM pg_collation) = 7 "
	"AND (SELECT count(*)=1 FROM pg_collation WHERE oid=100 AND collname='default' AND collnamespace=11 AND collowner=10 AND collprovider='d' AND collisdeterministic AND collencoding=-1 AND collcollate IS NULL AND collctype IS NULL AND colllocale IS NULL AND collicurules IS NULL AND collversion IS NULL) "
	"AND (SELECT count(*)=1 FROM pg_collation WHERE oid=950 AND collname='C' AND collnamespace=11 AND collowner=10 AND collprovider='c' AND collisdeterministic AND collencoding=-1 AND collcollate='C' AND collctype='C' AND colllocale IS NULL AND collicurules IS NULL AND collversion IS NULL) "
	"AND (SELECT count(*)=1 FROM pg_collation WHERE oid=951 AND collname='POSIX' AND collnamespace=11 AND collowner=10 AND collprovider='c' AND collisdeterministic AND collencoding=-1 AND collcollate='POSIX' AND collctype='POSIX' AND colllocale IS NULL AND collicurules IS NULL AND collversion IS NULL) "
	"AND (SELECT count(*)=1 FROM pg_collation WHERE oid=962 AND collname='ucs_basic' AND collnamespace=11 AND collowner=10 AND collprovider='b' AND collisdeterministic AND collencoding=6 AND collcollate IS NULL AND collctype IS NULL AND colllocale='C' AND collicurules IS NULL AND collversion='1') "
	"AND (SELECT count(*)=1 FROM pg_collation WHERE oid=963 AND collname='unicode' AND collnamespace=11 AND collowner=10 AND collprovider='i' AND collisdeterministic AND collencoding=-1 AND collcollate IS NULL AND collctype IS NULL AND colllocale='und' AND collicurules IS NULL AND collversion IS NULL) "
	"AND (SELECT count(*)=1 FROM pg_collation WHERE oid=811 AND collname='pg_c_utf8' AND collnamespace=11 AND collowner=10 AND collprovider='b' AND collisdeterministic AND collencoding=6 AND collcollate IS NULL AND collctype IS NULL AND colllocale='C.UTF-8' AND collicurules IS NULL AND collversion='1') "
	"AND (SELECT count(*)=1 FROM pg_collation WHERE oid=6411 AND collname='pg_unicode_fast' AND collnamespace=11 AND collowner=10 AND collprovider='b' AND collisdeterministic AND collencoding=6 AND collcollate IS NULL AND collctype IS NULL AND colllocale='PG_UNICODE_FAST' AND collicurules IS NULL AND collversion='1') "
	"AND (SELECT count(*) FROM pg_hba_file_rules) = 6 "
	"AND (SELECT count(*) FROM pg_hba_file_rules WHERE error IS NOT NULL) = 0 "
	"AND (SELECT count(*) FROM pg_hba_file_rules WHERE type='local' AND database='{all}' AND user_name='{all}' AND auth_method='peer') = 1 "
	"AND (SELECT count(*) FROM pg_hba_file_rules WHERE type='local' AND database='{replication}' AND user_name='{all}' AND auth_method='peer') = 1 "
	"AND (SELECT count(*) FROM pg_hba_file_rules WHERE type='host' AND database='{all}' AND user_name='{all}' AND address='127.0.0.1' AND netmask='255.255.255.255' AND auth_method='reject') = 1 "
	"AND (SELECT count(*) FROM pg_hba_file_rules WHERE type='host' AND database='{replication}' AND user_name='{all}' AND address='127.0.0.1' AND netmask='255.255.255.255' AND auth_method='reject') = 1 "
	"AND (SELECT count(*) FROM pg_hba_file_rules WHERE type='host' AND database='{all}' AND user_name='{all}' AND address='::1' AND auth_method='reject') = 1 "
	"AND (SELECT count(*) FROM pg_hba_file_rules WHERE type='host' AND database='{replication}' AND user_name='{all}' AND address='::1' AND auth_method='reject') = 1"
	") AS accepted";

int
main(int argc, char *argv[])
{
	const char *support_root;
	const char *data_root;
	const char *socket_root;
	const char *report_path;
	uid_t uid;
	gid_t gid;
	unsigned long port;
	char reference_stdout[PATH_MAX];
	char reference_stderr[PATH_MAX];
	char hostile_stdout[PATH_MAX];
	char hostile_stderr[PATH_MAX];
	char initdb_log[PATH_MAX];
	char postmaster_log[PATH_MAX];
	char port_text[16];
	pid_t backend_pid;
	int connection;
	bool escalated = false;
	enum wait_result stop_result;

	if (argc != 8)
	{
		fprintf(stderr, "usage: %s SUPPORT_ROOT PGDATA SOCKET_ROOT UID GID PORT REPORT_PATH\n", argv[0]);
		return 2;
	}
	support_root = argv[1];
	data_root = argv[2];
	socket_root = argv[3];
	uid = (uid_t) parse_number(argv[4], "UID", UINT32_MAX);
	gid = (gid_t) parse_number(argv[5], "GID", UINT32_MAX);
	port = parse_number(argv[6], "port", 65535);
	report_path = argv[7];
	require_absolute_path(support_root, "support root");
	require_absolute_path(data_root, "data root");
	require_absolute_path(socket_root, "socket root");
	require_absolute_path(report_path, "report path");
	if (uid == 0 || gid == 0 || getuid() != 0 || geteuid() != 0)
	{
		errno = 0;
		fail("lifecycle probe must start as root and drop to a non-root identity");
	}
	if (access(report_path, F_OK) == 0 || errno != ENOENT)
	{
		errno = 0;
		fail("lifecycle report path already exists or cannot be checked");
	}
	errno = 0;
	verify_identity("orna_kernel", uid, gid);
	require_directory(data_root, uid, gid, true);
	require_directory(socket_root, uid, gid, true);
	if (verify_support_directory(support_root, uid, gid) != EXPECTED_SUPPORT_FILES)
	{
		errno = 0;
		fail("support tree does not contain the accepted member count");
	}
	marker("preload-complete");
	drop_identity(uid, gid);

	join_path(reference_stdout, sizeof(reference_stdout), report_path, ".stdout");
	join_path(reference_stderr, sizeof(reference_stderr), report_path, ".reference.stderr");
	join_path(hostile_stdout, sizeof(hostile_stdout), report_path, ".hostile.stdout");
	join_path(hostile_stderr, sizeof(hostile_stderr), report_path, ".hostile.stderr");
	join_path(initdb_log, sizeof(initdb_log), report_path, ".initdb.log");
	join_path(postmaster_log, sizeof(postmaster_log), report_path, ".postmaster.log");
	if (setenv("PGSYSCONFDIR", "/hostile/system-configuration", 1) != 0 ||
		setenv("PGLOCALEDIR", "/hostile/locale-data", 1) != 0 ||
		setenv("PATH", "/hostile/path", 1) != 0)
		fail("could not set the hostile authority environment");
	marker("reference-entry-start");
	run_describe(support_root, "/usr/bin/orna", reference_stdout, reference_stderr);
	marker("reference-entry-complete");
	marker("hostile-entry-start");
	run_describe(support_root, "/hostile/untrusted-postgres", hostile_stdout, hostile_stderr);
	marker("hostile-entry-complete");
	require_empty_file(reference_stderr);
	require_empty_file(hostile_stderr);
	require_equal_files(reference_stdout, hostile_stdout);
	if (unlink(reference_stderr) != 0 || unlink(hostile_stdout) != 0 || unlink(hostile_stderr) != 0)
		fail("could not remove private describe-config captures");

	marker("initdb-start");
	run_initdb(support_root, data_root, initdb_log);
	marker("initdb-complete");
	marker("postmaster-start");
	if (snprintf(port_text, sizeof(port_text), "%lu", port) >= (int) sizeof(port_text))
	{
		errno = 0;
		fail("postmaster port text is too long");
	}
	live_postmaster = start_postmaster(support_root, data_root, socket_root, port_text, postmaster_log);
	connection = connect_postmaster(socket_root, port, live_postmaster, &backend_pid);
	marker("pgwire-ready");
	verify_process_identity(live_postmaster, backend_pid);
	send_query(connection, "CREATE DATABASE orna TEMPLATE template0");
	expect_create_database(connection);
	send_query(connection, assertion_query);
	expect_boolean_assertion(connection);
	marker("query-complete");
	if (close(connection) != 0)
		fail("could not close the private PostgreSQL connection");
	marker("postmaster-sigint");
	if (kill(live_postmaster, SIGINT) != 0)
		fail("could not request fast postmaster shutdown");
	stop_result = wait_for_stop(live_postmaster, FAST_STOP_ATTEMPTS);
	if (stop_result == WAIT_RESULT_FAILED)
	{
		errno = 0;
		fail("postmaster failed during fast shutdown");
	}
	if (stop_result == WAIT_RESULT_RUNNING)
	{
		escalated = true;
		marker("postmaster-sigquit");
		if (kill(live_postmaster, SIGQUIT) != 0)
			fail("could not request immediate postmaster shutdown");
		stop_result = wait_for_stop(live_postmaster, IMMEDIATE_STOP_ATTEMPTS);
		if (stop_result != WAIT_RESULT_CLEAN)
		{
			errno = 0;
			fail("postmaster did not stop after immediate escalation");
		}
	}
	if (unlink(initdb_log) != 0 || unlink(postmaster_log) != 0)
		fail("could not remove private PostgreSQL logs");
	write_report(report_path, escalated);
	marker("complete");
	return 0;
}
