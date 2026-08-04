// Cross-process futex over a MAP_SHARED /dev/shm page across fork() — the exact mechanism LTP's new-API
// tst_checkpoint uses (a shared IPC page of futex words; a child FUTEX_WAKEs, the parent FUTEX_WAITs, or
// vice-versa). This is the #402 shared-setup guard: LTP's tst_checkpoint_wake() loops until FUTEX_WAKE's
// return value EQUALS nr_wake, and hl's FUTEX_WAKE used to return the requested max (INT_MAX) instead of
// the ACTUAL number of waiters woken — so every checkpoint spun to ETIMEDOUT and BROKe the SETUP of
// pause01/pause02/mincore04/fork04 (and any *.needs_checkpoints test) before a single assertion ran.
//
// Deterministic self-check (hl must equal native on both arches):
//   * FUTEX_WAKE with NO waiter returns 0.
//   * With one child parked in FUTEX_WAIT on a shared-page word, FUTEX_WAKE(INT_MAX) returns exactly 1
//     (the count of waiters actually woken), and the child observes the wake.
// Output is booleans only (no addresses/pids), so `.oracle()` holds hl byte-identical to native/qemu.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/futex.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static long fwait(int *w, int val, struct timespec *to) {
	return syscall(SYS_futex, w, FUTEX_WAIT, val, to, NULL, 0);
}
static long fwake(int *w, int n) {
	return syscall(SYS_futex, w, FUTEX_WAKE, n, NULL, NULL, 0);
}

int main(void)
{
	size_t sz = getpagesize();
	char path[128];
	snprintf(path, sizeof path, "/dev/shm/ltp_cp_%d", (int)getpid());
	unlink(path);

	int fd = open(path, O_CREAT | O_EXCL | O_RDWR, 0600);
	if (fd < 0) {
		printf("open fail errno=%d\n", errno);
		return 2;
	}
	if (ftruncate(fd, sz) < 0) {
		printf("ftruncate fail errno=%d\n", errno);
		return 2;
	}
	/* MAP_SHARED file page — inherited (as one physical page) across a real fork; the futex word lives
	 * here so a WAKE in one process matches a WAIT in the other. */
	int *m = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (m == MAP_FAILED) {
		printf("mmap fail errno=%d\n", errno);
		return 2;
	}
	close(fd);
	m[0] = 0; /* the futex word */
	m[1] = 0; /* the child's "I was woken" marker */

	/* (1) no waiter parked -> WAKE returns 0. */
	long w0 = fwake(&m[0], 0x7fffffff);
	printf("wake_no_waiter_zero=%d\n", w0 == 0);

	pid_t pid = fork();
	if (pid < 0) {
		printf("fork fail errno=%d\n", errno);
		return 2;
	}
	if (pid == 0) {
		/* child: park on the shared word until the parent wakes us (bounded so a broken wake can't
		 * hang the test — 5s is far beyond the parent's wake latency). */
		struct timespec to = {5, 0};
		while (m[0] == 0) {
			long r = fwait(&m[0], 0, &to);
			if (r == -1 && (errno == ETIMEDOUT))
				break;
		}
		m[1] = 1; /* record that we returned from the wait */
		_exit(0);
	}

	/* parent: mirror tst_checkpoint_wake — loop WAKE until it reports it woke exactly nr_wake==1. */
	int woke_exactly_one = 0, tries = 0;
	for (;;) {
		long w = fwake(&m[0], 0x7fffffff);
		if (w == 1) {
			woke_exactly_one = 1;
			break;
		}
		if (w > 0)
			break; /* woke, but with the wrong count (the bug) */
		usleep(1000);
		if (++tries >= 5000)
			break; /* timed out (the bug: WAKE never reports a wakeup) */
	}
	m[0] = 1; /* release the child's re-check loop */
	fwake(&m[0], 0x7fffffff);

	int status = 0;
	waitpid(pid, &status, 0);
	printf("wake_returns_actual_count=%d\n", woke_exactly_one);
	printf("child_woken=%d\n", m[1] == 1);

	munmap(m, sz);
	unlink(path);
	return 0;
}
