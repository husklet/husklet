#include <stdint.h>

#if defined(__aarch64__)
#define GUEST_EPOLL_CREATE1 20
#define GUEST_EPOLL_CTL 21
#define GUEST_EPOLL_WAIT 22
#define GUEST_EVENTFD2 19
#define GUEST_CLOSE 57
#define GUEST_DUP 23
#define GUEST_READ 63
#define GUEST_WRITE 64
#define GUEST_EXIT 93
#define EPOLL_WAIT_ARGS 0, 8
#elif defined(__x86_64__)
#define GUEST_EPOLL_CREATE1 291
#define GUEST_EPOLL_CTL 233
#define GUEST_EPOLL_WAIT 232
#define GUEST_EVENTFD2 290
#define GUEST_CLOSE 3
#define GUEST_DUP 32
#define GUEST_READ 0
#define GUEST_WRITE 1
#define GUEST_EXIT 60
#define EPOLL_WAIT_ARGS 0, 0
#else
#error unsupported guest architecture
#endif

#define EPOLL_CTL_ADD 1
#define EPOLL_CTL_MOD 3
#define EPOLL_IN 1
#define EPOLL_ONESHOT (1U << 30)
#define EPOLL_EDGE (1U << 31)

#if defined(__x86_64__)
#define EPOLL_EVENT_LAYOUT __attribute__((packed))
#else
#define EPOLL_EVENT_LAYOUT
#endif
struct EPOLL_EVENT_LAYOUT epoll_event {
    uint32_t events;
    uint64_t data;
};

static long call6(long number, long first, long second, long third,
                  long fourth, long fifth, long sixth) {
#if defined(__aarch64__)
    register long x0 __asm__("x0") = first;
    register long x1 __asm__("x1") = second;
    register long x2 __asm__("x2") = third;
    register long x3 __asm__("x3") = fourth;
    register long x4 __asm__("x4") = fifth;
    register long x5 __asm__("x5") = sixth;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc 0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x3),
                     "r"(x4), "r"(x5), "r"(x8) : "memory");
    return x0;
#else
    register long r10 __asm__("r10") = fourth;
    register long r8 __asm__("r8") = fifth;
    register long r9 __asm__("r9") = sixth;
    long result;
    __asm__ volatile("syscall" : "=a"(result) : "a"(number), "D"(first),
                     "S"(second), "d"(third), "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return result;
#endif
}

__attribute__((noreturn)) static void guest_exit(long status) {
    call6(GUEST_EXIT, status, 0, 0, 0, 0, 0);
    __builtin_unreachable();
}

static long wait_event(long epoll, struct epoll_event *event) {
    return call6(GUEST_EPOLL_WAIT, epoll, (long)event, 1, 0,
                 EPOLL_WAIT_ARGS);
}

void _start(void) {
    static const char success[] = "epoll-ok\n";
    struct epoll_event interest = { EPOLL_IN, 0x1122334455667788ULL };
    struct epoll_event ready = { 0, 0 };
    uint64_t value = 1;

    long epoll = call6(GUEST_EPOLL_CREATE1, 0, 0, 0, 0, 0, 0);
    if (epoll < 0) guest_exit(10);
    long event = call6(GUEST_EVENTFD2, 0, 0, 0, 0, 0, 0);
    if (event < 0) guest_exit(11);
    if (call6(GUEST_EPOLL_CTL, epoll, EPOLL_CTL_ADD, event,
              (long)&interest, 0, 0) != 0) guest_exit(12);
    if (call6(GUEST_WRITE, event, (long)&value, sizeof(value), 0, 0, 0) !=
        (long)sizeof(value)) guest_exit(13);
    if (wait_event(epoll, (struct epoll_event *)1) != -14) guest_exit(14);
    long count = wait_event(epoll, &ready);
    if (count != 1) guest_exit(14);
    if (ready.events != EPOLL_IN) guest_exit(15);
    if (ready.data != interest.data) guest_exit(16);

    ready.events = 0;
    ready.data = 0;
    if (wait_event(epoll, &ready) != 1 || ready.events != EPOLL_IN ||
        ready.data != interest.data) guest_exit(17);
    if (call6(GUEST_READ, event, (long)&value, sizeof(value), 0, 0, 0) !=
        (long)sizeof(value)) guest_exit(18);
    for (unsigned int index = 0; index < 256; ++index)
        if (wait_event(epoll, &ready) != 0) guest_exit(19);

    interest.events = EPOLL_IN | EPOLL_EDGE;
    interest.data = 0x2233445566778899ULL;
    if (call6(GUEST_EPOLL_CTL, epoll, EPOLL_CTL_MOD, event,
              (long)&interest, 0, 0) != 0) guest_exit(20);
    if (call6(GUEST_WRITE, event, (long)&value, sizeof(value), 0, 0, 0) !=
        (long)sizeof(value)) guest_exit(21);
    if (wait_event(epoll, &ready) != 1 || ready.data != interest.data)
        guest_exit(22);
    if (wait_event(epoll, &ready) != 0) guest_exit(23);
    if (call6(GUEST_READ, event, (long)&value, sizeof(value), 0, 0, 0) !=
        (long)sizeof(value)) guest_exit(24);
    if (call6(GUEST_WRITE, event, (long)&value, sizeof(value), 0, 0, 0) !=
        (long)sizeof(value)) guest_exit(25);
    if (wait_event(epoll, &ready) != 1 || ready.data != interest.data)
        guest_exit(26);
    if (call6(GUEST_READ, event, (long)&value, sizeof(value), 0, 0, 0) !=
        (long)sizeof(value)) guest_exit(27);

    interest.events = EPOLL_IN | EPOLL_ONESHOT;
    interest.data = 0x33445566778899aaULL;
    if (call6(GUEST_EPOLL_CTL, epoll, EPOLL_CTL_MOD, event,
              (long)&interest, 0, 0) != 0) guest_exit(28);
    if (call6(GUEST_WRITE, event, (long)&value, sizeof(value), 0, 0, 0) !=
        (long)sizeof(value)) guest_exit(29);
    if (wait_event(epoll, &ready) != 1 || ready.data != interest.data)
        guest_exit(30);
    if (wait_event(epoll, &ready) != 0) guest_exit(31);
    if (call6(GUEST_EPOLL_CTL, epoll, EPOLL_CTL_MOD, event,
              (long)&interest, 0, 0) != 0) guest_exit(32);
    if (wait_event(epoll, &ready) != 1 || ready.data != interest.data)
        guest_exit(33);
    if (call6(GUEST_READ, event, (long)&value, sizeof(value), 0, 0, 0) !=
        (long)sizeof(value)) guest_exit(34);

    long retained = call6(GUEST_EVENTFD2, 0, 0, 0, 0, 0, 0);
    if (retained < 0) guest_exit(35);
    long alias = call6(GUEST_DUP, retained, 0, 0, 0, 0, 0);
    if (alias < 0) guest_exit(36);
    interest.events = EPOLL_IN;
    interest.data = 0x445566778899aabbULL;
    if (call6(GUEST_EPOLL_CTL, epoll, EPOLL_CTL_ADD, retained,
              (long)&interest, 0, 0) != 0) guest_exit(37);
    if (call6(GUEST_CLOSE, retained, 0, 0, 0, 0, 0) != 0) guest_exit(38);
    if (call6(GUEST_WRITE, alias, (long)&value, sizeof(value), 0, 0, 0) !=
        (long)sizeof(value)) guest_exit(39);
    if (wait_event(epoll, &ready) != 1 || ready.events != EPOLL_IN ||
        ready.data != interest.data) guest_exit(40);
    if (call6(GUEST_CLOSE, alias, 0, 0, 0, 0, 0) != 0) guest_exit(41);
    long final = wait_event(epoll, &ready);
    if (final != 0) {
        if (ready.data == interest.data) guest_exit(42);
        if (ready.data == 0x33445566778899aaULL) guest_exit(43);
        guest_exit(44);
    }

    call6(GUEST_WRITE, 1, (long)success, sizeof(success) - 1, 0, 0, 0);
    guest_exit(0);
}
