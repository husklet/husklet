#if defined(__x86_64__)
#define NR_OPENAT 257
#define NR_CLOSE 3
#define NR_READ 0
#define NR_WRITE 1
#define NR_READLINKAT 267
#define NR_EXIT 60
static long call6(long n, long a, long b, long c, long d, long e, long f) {
  long result;
  register long r10 __asm__("r10") = d;
  register long r8 __asm__("r8") = e;
  register long r9 __asm__("r9") = f;
  __asm__ volatile("syscall"
                   : "=a"(result)
                   : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                   : "rcx", "r11", "memory");
  return result;
}
#elif defined(__aarch64__)
#define NR_OPENAT 56
#define NR_CLOSE 57
#define NR_READ 63
#define NR_WRITE 64
#define NR_READLINKAT 78
#define NR_EXIT 93
static long call6(long n, long a, long b, long c, long d, long e, long f) {
  register long x8 __asm__("x8") = n;
  register long x0 __asm__("x0") = a;
  register long x1 __asm__("x1") = b;
  register long x2 __asm__("x2") = c;
  register long x3 __asm__("x3") = d;
  register long x4 __asm__("x4") = e;
  register long x5 __asm__("x5") = f;
  __asm__ volatile("svc 0"
                   : "+r"(x0)
                   : "r"(x8), "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5)
                   : "memory");
  return x0;
}
#else
#error unsupported guest architecture
#endif

typedef unsigned long usize;

static long call1(long n, long a) { return call6(n, a, 0, 0, 0, 0, 0); }
static long call3(long n, long a, long b, long c) {
  return call6(n, a, b, c, 0, 0, 0);
}
static long call4(long n, long a, long b, long c, long d) {
  return call6(n, a, b, c, d, 0, 0);
}
static int same(const char *a, const char *b, usize n) {
  for (usize i = 0; i < n; i++)
    if (a[i] != b[i])
      return 0;
  return 1;
}
static void fail(long code) { call1(NR_EXIT, code); }

void _start(void) {
  char data[16], link[32];
  long fd = call4(NR_OPENAT, -100, (long)"/data", 0, 0);
  if (fd < 0 || call3(NR_READ, fd, (long)data, 8) != 8 ||
      !same(data, "original", 8))
    fail(1);
  call1(NR_CLOSE, fd);

  /* The projected root is the complete namespace. Neither an absolute host
     path nor excessive `..` may fall through to the worker's ambient root. */
  if (call4(NR_OPENAT, -100, (long)"/etc/passwd", 0, 0) != -2)
    fail(2);
  if (call4(NR_OPENAT, -100, (long)"/../../etc/passwd", 0, 0) != -2)
    fail(3);

  /* Read-only projection is authority policy, not merely an open-handle
     property: reopening a present object for write must be rejected. */
  if (call4(NR_OPENAT, -100, (long)"/data", 1, 0) != -30)
    fail(4);

  long count =
      call4(NR_READLINKAT, -100, (long)"/proc/self/exe", (long)link, 32);
  if (count != 6 || !same(link, "/guest", 6))
    fail(5);
  call3(NR_WRITE, 1, (long)"projected-file-ok\n", 18);
  call1(NR_EXIT, 0);
}
