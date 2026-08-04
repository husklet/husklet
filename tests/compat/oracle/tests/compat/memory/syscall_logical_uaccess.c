#define _GNU_SOURCE
#include <errno.h>
#include <arpa/inet.h>
#include <fcntl.h>
#include <linux/aio_abi.h>
#include <linux/filter.h>
#include <linux/netlink.h>
#include <linux/openat2.h>
#include <linux/rtnetlink.h>
#include <linux/seccomp.h>
#include <linux/stat.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <mqueue.h>
#include <sched.h>
#include <sys/ioctl.h>
#include <sys/msg.h>
#include <sys/epoll.h>
#include <sys/select.h>
#include <sys/sem.h>
#include <sys/shm.h>
#include <sys/socket.h>
#include <sys/signalfd.h>
#include <sys/stat.h>
#include <sys/timerfd.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/times.h>
#include <sys/resource.h>
#include <sys/prctl.h>
#include <sys/time.h>
#include <sys/sysinfo.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <sys/vfs.h>
#include <sys/uio.h>
#include <time.h>
#include <unistd.h>

static int fill_pipe(int pipefd[2], unsigned char seed, size_t length) {
    unsigned char bytes[8192];
    for (size_t i = 0; i < length; ++i)
        bytes[i] = (unsigned char)(seed + i);
    return write(pipefd[1], bytes, length) == (ssize_t)length;
}

int main(void) {
    const size_t page = 4096;
    int memfd = (int)syscall(SYS_memfd_create, "logical-uaccess", 0u);
    if (memfd < 0 || ftruncate(memfd, 4 * (off_t)page) != 0) return 2;

    unsigned char *reservation = mmap(NULL, 5 * page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (reservation == MAP_FAILED) return 3;
    unsigned char *soft0 = mmap(reservation + page, page, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, memfd, page);
    unsigned char *direct =
        mmap(reservation + 2 * page, page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    unsigned char *soft1 =
        mmap(reservation + 3 * page, page, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, memfd, 2 * page);
    if (soft0 != reservation + page || direct != reservation + 2 * page || soft1 != reservation + 3 * page) return 4;

    int p[2];
    if (pipe(p) != 0 || !fill_pipe(p, 7, 2 * page)) return 5;
    ssize_t cross = read(p[0], soft0, 2 * page);
    int cross_ok = cross == (ssize_t)(2 * page) && soft0[0] == 7 && direct[page - 1] == (unsigned char)(6);
    close(p[0]);
    close(p[1]);

    struct iovec source[2] = {{soft0, page}, {soft1, page}};
    int out[2];
    unsigned char verify[8192];
    if (pipe(out) != 0) return 6;
    ssize_t gathered = writev(out[1], source, 2);
    ssize_t got = read(out[0], verify, sizeof verify);
    int aliases_ok = gathered == (ssize_t)(2 * page) && got == gathered && memcmp(verify, soft0, page) == 0 &&
                     memcmp(verify + page, soft1, page) == 0;
    close(out[0]);
    close(out[1]);

    /* The first page is writable canonical storage; the second is PROT_NONE. */
    if (munmap(direct, page) != 0 ||
        mmap(direct, page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0) != direct)
        return 7;
    if (pipe(p) != 0 || !fill_pipe(p, 31, 2 * page)) return 8;
    errno = 0;
    ssize_t partial = read(p[0], soft0, 2 * page);
    unsigned char next = 0;
    ssize_t remainder = read(p[0], &next, 1);
    int fault_ok = partial == (ssize_t)page && remainder == 1 && next == 31;
    close(p[0]);
    close(p[1]);

    struct iovec zero_then_fault[2] = {{NULL, 0}, {direct, 1}};
    if (pipe(p) != 0) return 9;
    errno = 0;
    int zero_fault_ok = writev(p[1], zero_then_fault, 2) < 0 && errno == EFAULT;
    close(p[0]);
    close(p[1]);

    struct timespec *now = (struct timespec *)(soft0 + 128);
    struct timespec *resolution = (struct timespec *)(soft0 + 160);
    struct timeval *wall = (struct timeval *)(soft0 + 192);
    struct tms *cpu = (struct tms *)(soft0 + 224);
    struct timespec *request = (struct timespec *)(soft1 + 128);
    int *timer_id = (int *)(soft0 + 640);
    unsigned char *new_timer = soft1 + 640;
    unsigned char *old_timer = soft1 + 672;
    unsigned char *current_timer = soft0 + 672;
    *request = (struct timespec){.tv_nsec = 1};
    memset(new_timer, 0, 32);
    int time_ok = syscall(SYS_clock_gettime, CLOCK_MONOTONIC, now) == 0 && now->tv_sec >= 0 &&
                  syscall(SYS_clock_getres, CLOCK_MONOTONIC, resolution) == 0 && resolution->tv_nsec > 0 &&
                  syscall(SYS_gettimeofday, wall, NULL) == 0 && wall->tv_sec > 0 && syscall(SYS_times, cpu) >= 0 &&
                  syscall(SYS_nanosleep, request, NULL) == 0 &&
                  syscall(SYS_clock_nanosleep, CLOCK_MONOTONIC, 0, request, NULL) == 0 &&
                  syscall(SYS_timer_create, CLOCK_MONOTONIC, NULL, timer_id) == 0 &&
                  syscall(SYS_timer_settime, *timer_id, 0, new_timer, old_timer) == 0 &&
                  syscall(SYS_timer_gettime, *timer_id, current_timer) == 0 &&
                  syscall(SYS_timer_delete, *timer_id) == 0;

    uint64_t *set = (uint64_t *)(soft0 + 288);
    uint64_t *oldset = (uint64_t *)(soft0 + 296);
    uint64_t *pending = (uint64_t *)(soft0 + 304);
    uint64_t *action = (uint64_t *)(soft1 + 288);
    uint64_t *oldaction = (uint64_t *)(soft1 + 320);
    unsigned char *siginfo = soft1 + 352;
    unsigned char *new_stack = soft1 + 480;
    unsigned char *old_stack = soft1 + 512;
    uint64_t *waitset = (uint64_t *)(soft0 + 320);
    unsigned char *waitinfo = soft0 + 336;
    struct timespec *zero_timeout = (struct timespec *)(soft1 + 544);
    *set = 1ull << (SIGUSR1 - 1);
    memset(action, 0, 32);
    action[0] = 1; /* SIG_IGN */
    memset(siginfo, 0, 128);
    *(int *)(siginfo + 8) = -1;       /* SI_QUEUE */
    *(uint32_t *)(new_stack + 8) = 2; /* SS_DISABLE */
    *waitset = 0;
    *zero_timeout = (struct timespec){0};
    int signal_ok = syscall(SYS_rt_sigprocmask, SIG_BLOCK, set, oldset, 8) == 0 &&
                    syscall(SYS_rt_sigpending, pending, 8) == 0 &&
                    syscall(SYS_rt_sigaction, SIGUSR1, action, oldaction, 8) == 0 &&
                    syscall(SYS_rt_sigqueueinfo, getpid(), 0, siginfo) == 0 &&
                    syscall(SYS_sigaltstack, new_stack, old_stack) == 0 &&
                    (errno = 0, syscall(SYS_rt_sigtimedwait, waitset, waitinfo, zero_timeout, 8) < 0) &&
                    errno == EAGAIN && syscall(SYS_rt_sigaction, SIGUSR1, oldaction, NULL, 8) == 0 &&
                    syscall(SYS_rt_sigprocmask, SIG_SETMASK, oldset, NULL, 8) == 0;

    int event_pipe[2];
    int ep = epoll_create1(0);
    struct epoll_event *ctl_event = (struct epoll_event *)(soft1 + 768);
    struct epoll_event *ready_event = (struct epoll_event *)(soft0 + 768);
    struct pollfd *poll_event = (struct pollfd *)(soft1 + 832);
    unsigned long *read_bits = (unsigned long *)(soft0 + 832);
    struct timespec *event_timeout = (struct timespec *)(soft1 + 864);
    uint64_t *event_mask = (uint64_t *)(soft0 + 864);
    uint64_t *pselect_pair = (uint64_t *)(soft0 + 880);
    int event_ok = 0;
    if (ep >= 0 && pipe(event_pipe) == 0) {
        memset(ctl_event, 0, sizeof(*ctl_event));
        ctl_event->events = EPOLLIN;
        ctl_event->data.u64 = UINT64_C(0x123456789abcdef0);
        *event_timeout = (struct timespec){0};
        *event_mask = 0;
        pselect_pair[0] = (uint64_t)(uintptr_t)event_mask;
        pselect_pair[1] = 8;
        poll_event[0] = (struct pollfd){.fd = event_pipe[0], .events = POLLIN};
        *read_bits = 1ul << event_pipe[0];
        char byte = 'e';
        int ctl_ok = syscall(SYS_epoll_ctl, ep, EPOLL_CTL_ADD, event_pipe[0], ctl_event) == 0;
        int write_ok = write(event_pipe[1], &byte, 1) == 1;
        int epoll_ok = syscall(SYS_epoll_pwait2, ep, ready_event, 1, event_timeout, event_mask, 8) == 1 &&
                       ready_event->events == EPOLLIN && ready_event->data.u64 == UINT64_C(0x123456789abcdef0);
        long ppoll_result = syscall(SYS_ppoll, poll_event, 1, event_timeout, event_mask, 8);
        int ppoll_ok = ppoll_result == 1 && (poll_event[0].revents & POLLIN) != 0;
        int pselect_ok =
            syscall(SYS_pselect6, event_pipe[0] + 1, read_bits, NULL, NULL, event_timeout, pselect_pair) == 1 &&
            (*read_bits & (1ul << event_pipe[0])) != 0;
        uint64_t *signal_fd_mask = (uint64_t *)(soft1 + 928);
        struct itimerspec *timer_new = (struct itimerspec *)(soft1 + 960);
        struct itimerspec *timer_old = (struct itimerspec *)(soft0 + 960);
        struct itimerspec *timer_current = (struct itimerspec *)(soft0 + 1024);
        *signal_fd_mask = 0;
        *timer_new = (struct itimerspec){0};
        int signal_fd = (int)syscall(SYS_signalfd4, -1, signal_fd_mask, 8, SFD_NONBLOCK);
        int timer_fd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
        int signal_fd_ok = signal_fd >= 0;
        int timer_fd_ok = timer_fd >= 0 && timerfd_settime(timer_fd, 0, timer_new, timer_old) == 0 &&
                          timerfd_gettime(timer_fd, timer_current) == 0 && timer_current->it_value.tv_sec == 0;
        unsigned char *udp_payload = soft1 + 1088;
        unsigned char *udp_receive = soft0 + 1088;
        struct sockaddr_in *udp_destination = (struct sockaddr_in *)(soft1 + 1152);
        struct sockaddr_in *udp_source = (struct sockaddr_in *)(soft0 + 1152);
        socklen_t *udp_source_length = (socklen_t *)(soft0 + 1184);
        int *socket_option = (int *)(soft1 + 1216);
        socklen_t *socket_option_length = (socklen_t *)(soft0 + 1216);
        int udp_receiver = socket(AF_INET, SOCK_DGRAM, 0);
        int udp_sender = socket(AF_INET, SOCK_DGRAM, 0);
        struct sockaddr_in udp_bound = {.sin_family = AF_INET, .sin_addr.s_addr = htonl(INADDR_LOOPBACK)};
        socklen_t udp_bound_length = sizeof(udp_bound);
        memcpy(udp_payload, "logical-udp", 11);
        *udp_destination = udp_bound;
        int udp_ok = udp_receiver >= 0 && udp_sender >= 0 &&
                     bind(udp_receiver, (struct sockaddr *)udp_destination, sizeof(*udp_destination)) == 0 &&
                     getsockname(udp_receiver, (struct sockaddr *)&udp_bound, &udp_bound_length) == 0;
        *udp_destination = udp_bound;
        *udp_source_length = sizeof(*udp_source);
        *socket_option = 1;
        *socket_option_length = sizeof(*socket_option);
        udp_ok = udp_ok && connect(udp_sender, (struct sockaddr *)udp_destination, sizeof(*udp_destination)) == 0 &&
                 setsockopt(udp_sender, SOL_SOCKET, SO_REUSEADDR, socket_option, sizeof(*socket_option)) == 0 &&
                 (*socket_option = 0,
                  getsockopt(udp_sender, SOL_SOCKET, SO_REUSEADDR, socket_option, socket_option_length)) == 0 &&
                 *socket_option != 0 && *socket_option_length == sizeof(*socket_option) &&
                 getsockname(udp_receiver, (struct sockaddr *)udp_source, udp_source_length) == 0 &&
                 udp_source->sin_family == AF_INET &&
                 (*udp_source_length = sizeof(*udp_source),
                  getpeername(udp_sender, (struct sockaddr *)udp_source, udp_source_length)) == 0 &&
                 udp_source->sin_family == AF_INET && (*udp_source_length = sizeof(*udp_source), 1) &&
                 sendto(udp_sender, udp_payload, 11, 0, (struct sockaddr *)udp_destination, sizeof(*udp_destination)) ==
                     11 &&
                 recvfrom(udp_receiver, udp_receive, 11, 0, (struct sockaddr *)udp_source, udp_source_length) == 11 &&
                 memcmp(udp_receive, "logical-udp", 11) == 0 && udp_source->sin_family == AF_INET;
        struct msghdr *send_header = (struct msghdr *)(soft1 + 1280);
        struct iovec *send_vector = (struct iovec *)(soft1 + 1344);
        unsigned char *message_payload = soft1 + 1408;
        unsigned char *message_payload_alias = soft0 + 1472;
        struct msghdr *receive_header = (struct msghdr *)(soft0 + 1280);
        struct iovec *receive_vector = (struct iovec *)(soft0 + 1344);
        unsigned char *message_receive = soft0 + 1408;
        unsigned char *message_receive_alias = soft1 + 1472;
        memcpy(message_payload, "neste", 5);
        memcpy(message_payload_alias, "d-msg", 5);
        send_vector[0] = (struct iovec){.iov_base = message_payload, .iov_len = 5};
        send_vector[1] = (struct iovec){.iov_base = message_payload_alias, .iov_len = 5};
        *send_header = (struct msghdr){.msg_name = udp_destination,
                                       .msg_namelen = sizeof(*udp_destination),
                                       .msg_iov = send_vector,
                                       .msg_iovlen = 2};
        *udp_source_length = sizeof(*udp_source);
        receive_vector[0] = (struct iovec){.iov_base = message_receive, .iov_len = 5};
        receive_vector[1] = (struct iovec){.iov_base = message_receive_alias, .iov_len = 5};
        *receive_header = (struct msghdr){
            .msg_name = udp_source, .msg_namelen = sizeof(*udp_source), .msg_iov = receive_vector, .msg_iovlen = 2};
        int message_ok = sendmsg(udp_sender, send_header, 0) == 10 && recvmsg(udp_receiver, receive_header, 0) == 10 &&
                         memcmp(message_receive, "neste", 5) == 0 && memcmp(message_receive_alias, "d-msg", 5) == 0 &&
                         receive_header->msg_namelen >= sizeof(struct sockaddr_in);
        send_header->msg_control = direct;
        send_header->msg_controllen = 16;
        errno = 0;
        message_ok = message_ok && sendmsg(udp_sender, send_header, 0) < 0 && errno == EFAULT;
        struct mmsghdr *send_messages = (struct mmsghdr *)(soft1 + 1536);
        struct mmsghdr *receive_messages = (struct mmsghdr *)(soft0 + 1536);
        struct iovec *send_message_vectors = (struct iovec *)(soft1 + 1680);
        struct iovec *receive_message_vectors = (struct iovec *)(soft0 + 1680);
        memcpy(message_payload, "one!!", 5);
        memcpy(message_payload_alias, "two!!", 5);
        send_message_vectors[0] = (struct iovec){message_payload, 5};
        send_message_vectors[1] = (struct iovec){message_payload_alias, 5};
        memset(send_messages, 0, 2 * sizeof(*send_messages));
        for (int mi = 0; mi < 2; ++mi) {
            send_messages[mi].msg_hdr.msg_name = udp_destination;
            send_messages[mi].msg_hdr.msg_namelen = sizeof(*udp_destination);
            send_messages[mi].msg_hdr.msg_iov = &send_message_vectors[mi];
            send_messages[mi].msg_hdr.msg_iovlen = 1;
        }
        receive_message_vectors[0] = (struct iovec){message_receive, 5};
        receive_message_vectors[1] = (struct iovec){message_receive_alias, 5};
        memset(receive_messages, 0, 2 * sizeof(*receive_messages));
        for (int mi = 0; mi < 2; ++mi) {
            receive_messages[mi].msg_hdr.msg_iov = &receive_message_vectors[mi];
            receive_messages[mi].msg_hdr.msg_iovlen = 1;
        }
        int mmsg_ok = sendmmsg(udp_sender, send_messages, 2, 0) == 2 &&
                      recvmmsg(udp_receiver, receive_messages, 2, 0, NULL) == 2 && receive_messages[0].msg_len == 5 &&
                      receive_messages[1].msg_len == 5 && memcmp(message_receive, "one!!", 5) == 0 &&
                      memcmp(message_receive_alias, "two!!", 5) == 0;
        unsigned char *netlink_request = soft1 + 1856;
        struct sockaddr_nl *netlink_address = (struct sockaddr_nl *)(soft1 + 1920);
        struct msghdr *netlink_send_header = (struct msghdr *)(soft1 + 1984);
        struct iovec *netlink_send_vector = (struct iovec *)(soft1 + 2048);
        struct msghdr *netlink_receive_header = (struct msghdr *)(soft0 + 1984);
        struct iovec *netlink_receive_vector = (struct iovec *)(soft0 + 2048);
        struct sockaddr_nl *netlink_source = (struct sockaddr_nl *)(soft0 + 2080);
        socklen_t *netlink_source_length = (socklen_t *)(soft0 + 2120);
        unsigned char *netlink_response = soft0 + 2304;
        struct nlmsghdr *netlink_message = (struct nlmsghdr *)netlink_request;
        struct ifinfomsg *netlink_info = (struct ifinfomsg *)(netlink_message + 1);
        memset(netlink_request, 0, NLMSG_LENGTH(sizeof(*netlink_info)));
        netlink_message->nlmsg_len = NLMSG_LENGTH(sizeof(*netlink_info));
        netlink_message->nlmsg_type = RTM_GETLINK;
        netlink_message->nlmsg_flags = NLM_F_REQUEST | NLM_F_DUMP;
        *netlink_address = (struct sockaddr_nl){.nl_family = AF_NETLINK};
        int netlink_fd = socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE);
        socklen_t netlink_address_length = sizeof(*netlink_address);
        int netlink_ok = netlink_fd >= 0 &&
                         bind(netlink_fd, (struct sockaddr *)netlink_address, sizeof(*netlink_address)) == 0 &&
                         getsockname(netlink_fd, (struct sockaddr *)netlink_address, &netlink_address_length) == 0;
        *netlink_send_vector = (struct iovec){netlink_request, netlink_message->nlmsg_len};
        *netlink_send_header = (struct msghdr){.msg_name = netlink_address,
                                               .msg_namelen = sizeof(*netlink_address),
                                               .msg_iov = netlink_send_vector,
                                               .msg_iovlen = 1};
        *netlink_receive_vector = (struct iovec){netlink_response, 1024};
        *netlink_receive_header = (struct msghdr){.msg_name = netlink_source,
                                                  .msg_namelen = sizeof(*netlink_source),
                                                  .msg_iov = netlink_receive_vector,
                                                  .msg_iovlen = 1};
        netlink_ok = netlink_ok && sendmsg(netlink_fd, netlink_send_header, 0) == (ssize_t)netlink_message->nlmsg_len &&
                     recvmsg(netlink_fd, netlink_receive_header, 0) > 0 &&
                     ((struct nlmsghdr *)netlink_response)->nlmsg_len >= sizeof(struct nlmsghdr) &&
                     netlink_source->nl_family == AF_NETLINK;
        *netlink_source_length = sizeof(*netlink_source);
        netlink_ok =
            netlink_ok &&
            send(netlink_fd, netlink_request, netlink_message->nlmsg_len, 0) == (ssize_t)netlink_message->nlmsg_len &&
            recvfrom(netlink_fd, netlink_response, 1024, 0, (struct sockaddr *)netlink_source, netlink_source_length) >
                0 &&
            netlink_source->nl_family == AF_NETLINK;
        netlink_send_header->msg_control = direct;
        netlink_send_header->msg_controllen = 16;
        errno = 0;
        netlink_ok = netlink_ok && sendmsg(netlink_fd, netlink_send_header, 0) < 0 && errno == EFAULT;
        if (netlink_fd >= 0) close(netlink_fd);
        aio_context_t *aio_context = (aio_context_t *)(soft0 + 3392);
        struct iocb *aio_control = (struct iocb *)(soft1 + 3392);
        struct iocb **aio_controls = (struct iocb **)(soft0 + 3472);
        struct io_event *aio_event = (struct io_event *)(soft1 + 3472);
        unsigned char *aio_buffer = soft0 + 3552;
        struct timespec *aio_timeout = (struct timespec *)(soft1 + 3552);
        *aio_context = 0;
        memset(aio_control, 0, sizeof(*aio_control));
        aio_control->aio_lio_opcode = IOCB_CMD_PREAD;
        aio_control->aio_fildes = (uint32_t)memfd;
        aio_control->aio_buf = (uint64_t)(uintptr_t)aio_buffer;
        aio_control->aio_nbytes = 8;
        aio_control->aio_offset = 0;
        aio_control->aio_data = UINT64_C(0xa10a);
        *aio_controls = aio_control;
        *aio_timeout = (struct timespec){0};
        int aio_ok = pwrite(memfd, "aio-data", 8, 0) == 8 && syscall(SYS_io_setup, 4, aio_context) == 0;
        errno = 0;
        aio_ok = aio_ok && syscall(SYS_io_submit, *aio_context, 1, direct) < 0 && errno == EFAULT &&
                 syscall(SYS_io_submit, *aio_context, 1, aio_controls) == 1 &&
                 syscall(SYS_io_getevents, *aio_context, 1, 1, aio_event, aio_timeout) == 1 &&
                 aio_event->data == UINT64_C(0xa10a) && aio_event->res == 8 && memcmp(aio_buffer, "aio-data", 8) == 0 &&
                 syscall(SYS_io_destroy, *aio_context) == 0;

        struct {
            long type;
            char text[16];
        } *message_send = (void *)(soft1 + 3648), *message_receive_sysv = (void *)(soft0 + 3648);
        struct msqid_ds *message_status = (struct msqid_ds *)(soft1 + 3712);
        message_send->type = 7;
        memcpy(message_send->text, "sysv-message", 13);
        int message_queue = msgget(IPC_PRIVATE, IPC_CREAT | 0600);
        int sysv_ok = message_queue >= 0 && msgsnd(message_queue, message_send, 13, IPC_NOWAIT) == 0 &&
                      msgrcv(message_queue, message_receive_sysv, 16, 7, IPC_NOWAIT) == 13 &&
                      message_receive_sysv->type == 7 && memcmp(message_receive_sysv->text, "sysv-message", 13) == 0 &&
                      msgctl(message_queue, IPC_STAT, message_status) == 0;
        errno = 0;
        sysv_ok = sysv_ok && msgsnd(message_queue, direct, 1, IPC_NOWAIT) < 0 && errno == EFAULT;
        if (message_queue >= 0) msgctl(message_queue, IPC_RMID, NULL);
        unsigned short *semaphore_values = (unsigned short *)(soft1 + 2304);
        unsigned short *semaphore_readback = (unsigned short *)(soft0 + 3350);
        struct sembuf *semaphore_operation = (struct sembuf *)(soft1 + 2336);
        struct semid_ds *semaphore_status = (struct semid_ds *)(soft1 + 2400);

        union {
            int val;
            unsigned short *array;
            struct semid_ds *buf;
        } semaphore_argument;

        int semaphore = semget(IPC_PRIVATE, 2, IPC_CREAT | 0600);
        semaphore_values[0] = 1;
        semaphore_values[1] = 2;
        semaphore_argument.array = semaphore_values;
        *semaphore_operation = (struct sembuf){.sem_num = 0, .sem_op = -1, .sem_flg = IPC_NOWAIT};
        int semaphore_ok = semaphore >= 0 && semctl(semaphore, 0, SETALL, semaphore_argument) == 0 &&
                           semop(semaphore, semaphore_operation, 1) == 0;
        semaphore_argument.array = semaphore_readback;
        semaphore_ok = semaphore_ok && semctl(semaphore, 0, GETALL, semaphore_argument) == 0 &&
                       semaphore_readback[0] == 0 && semaphore_readback[1] == 2;
        semaphore_argument.buf = semaphore_status;
        semaphore_ok = semaphore_ok && semctl(semaphore, 0, IPC_STAT, semaphore_argument) == 0;
        errno = 0;
        semaphore_ok = semaphore_ok && semop(semaphore, (struct sembuf *)direct, 1) < 0 && errno == EFAULT;
        if (semaphore >= 0) semctl(semaphore, 0, IPC_RMID);

        struct shmid_ds *shared_status = (struct shmid_ds *)(soft1 + 2528);
        int shared_segment = shmget(IPC_PRIVATE, 4096, IPC_CREAT | 0600);
        int shared_ok = shared_segment >= 0 && shmctl(shared_segment, IPC_STAT, shared_status) == 0 &&
                        shared_status->shm_segsz == 4096;
        errno = 0;
        shared_ok = shared_ok && shmctl(shared_segment, IPC_STAT, (struct shmid_ds *)direct) < 0 && errno == EFAULT;
        if (shared_segment >= 0) shmctl(shared_segment, IPC_RMID, NULL);
        char *queue_name = (char *)(soft1 + 2784);
        struct mq_attr *queue_attribute = (struct mq_attr *)(soft1 + 2848);
        struct mq_attr *queue_old_attribute = (struct mq_attr *)(soft0 + 2784);
        char *queue_send = (char *)(soft1 + 2944);
        char *queue_receive = (char *)(soft0 + 2944);
        unsigned *queue_priority = (unsigned *)(soft0 + 3008);
        snprintf(queue_name, 48, "/hl-logical-%d", getpid());
        *queue_attribute = (struct mq_attr){.mq_maxmsg = 4, .mq_msgsize = 32};
        memcpy(queue_send, "queue-data", 10);
        mqd_t queue = mq_open(queue_name, O_CREAT | O_EXCL | O_RDWR | O_NONBLOCK, 0600, queue_attribute);
        int queue_ok = queue >= 0 && mq_getattr(queue, queue_old_attribute) == 0 &&
                       mq_send(queue, queue_send, 10, 3) == 0 &&
                       mq_receive(queue, queue_receive, 32, queue_priority) == 10 &&
                       memcmp(queue_receive, "queue-data", 10) == 0 && *queue_priority == 3;
        errno = 0;
        queue_ok = queue_ok && mq_send(queue, direct, 1, 0) < 0 && errno == EFAULT;
        if (queue >= 0) mq_close(queue);
        mq_unlink(queue_name);

        uint32_t *cap_header = (uint32_t *)(soft0 + 3072);
        uint32_t *cap_data = (uint32_t *)(soft1 + 3072);
        cap_header[0] = 0x20080522;
        cap_header[1] = 0;
        memset(cap_data, 0, 24);
        int cap_ok = syscall(SYS_capget, cap_header, cap_data) == 0 && cap_data[1] != 0;
        errno = 0;
        cap_ok = cap_ok && syscall(SYS_capget, cap_header, direct) < 0 && errno == EFAULT;

        unsigned char *cpu_mask = soft1 + 3136;
        memset(cpu_mask, 0, 128);
        long affinity_bytes = syscall(SYS_sched_getaffinity, 0, 128, cpu_mask);
        int affinity_ok = affinity_bytes > 0 && syscall(SYS_sched_setaffinity, 0, 128, cpu_mask) == 0;
        errno = 0;
        affinity_ok =
            affinity_ok && syscall(SYS_sched_getaffinity, 0, 128, direct) < 0 && errno == EFAULT;

        struct sched_param *schedule = (struct sched_param *)(soft0 + 3136);
        memset(schedule, 0, sizeof(*schedule));
        int schedule_ok = syscall(SYS_sched_getparam, 0, schedule) == 0 &&
                          syscall(SYS_sched_setparam, 0, schedule) == 0;
        uint32_t *ruid = (uint32_t *)(soft0 + 3168);
        uint32_t *euid = (uint32_t *)(soft1 + 3280);
        uint32_t *suid = (uint32_t *)(soft0 + 3172);
        int ids_ok = syscall(SYS_getresuid, ruid, euid, suid) == 0 && *ruid == getuid() && *euid == geteuid();

        gid_t *groups = (gid_t *)(soft0 + 3328);
        int group_count = (int)syscall(SYS_getgroups, 0, NULL);
        int groups_ok = group_count >= 0 && group_count <= 128 &&
                        syscall(SYS_getgroups, 128, groups) == group_count;

        struct rusage *usage = (struct rusage *)(soft1 + 3328);
        int usage_ok = syscall(SYS_getrusage, RUSAGE_SELF, usage) == 0;
        pid_t wait_child = fork();
        if (wait_child == 0) _exit(23);
        int *wait_status = (int *)(soft0 + 3264);
        struct rusage *wait_usage = (struct rusage *)(soft1 + 3520);
        int wait_ok = wait_child > 0 && syscall(SYS_wait4, wait_child, wait_status, 0, wait_usage) == wait_child &&
                      WIFEXITED(*wait_status) && WEXITSTATUS(*wait_status) == 23;
        int proc_ok = cap_ok && affinity_ok && schedule_ok && ids_ok && groups_ok && usage_ok && wait_ok;

        char *filesystem_path = (char *)(soft0 + 3904);
        struct stat *filesystem_status = (struct stat *)(soft1 + 3800);
        memcpy(filesystem_path, "/dev/null", 10);
        int filesystem_fd = (int)syscall(SYS_openat, AT_FDCWD, filesystem_path, O_RDONLY, 0);
        int filesystem_ok = filesystem_fd >= 0 && syscall(SYS_fstat, filesystem_fd, filesystem_status) == 0 &&
                            S_ISCHR(filesystem_status->st_mode) &&
                            syscall(SYS_newfstatat, AT_FDCWD, filesystem_path, filesystem_status, 0) == 0;
        errno = 0;
        filesystem_ok = filesystem_ok &&
                        syscall(SYS_newfstatat, AT_FDCWD, filesystem_path, direct, 0) < 0 && errno == EFAULT;
        errno = 0;
        filesystem_ok =
            filesystem_ok && syscall(SYS_openat, AT_FDCWD, direct, O_RDONLY, 0) < 0 && errno == EFAULT;
        char *cwd_result = (char *)soft0;
        filesystem_ok = filesystem_ok && syscall(SYS_getcwd, cwd_result, 512) > 0 && cwd_result[0] == '/';
        memcpy(filesystem_path, "/proc/self", 11);
        char *link_result = (char *)soft1;
        filesystem_ok = filesystem_ok &&
                        syscall(SYS_readlinkat, AT_FDCWD, filesystem_path, link_result, 64) > 0;
        memcpy(filesystem_path, "/dev/null", 10);
        struct statx *extended_status = (struct statx *)(soft1 + 256);
        filesystem_ok = filesystem_ok &&
                        syscall(SYS_statx, AT_FDCWD, filesystem_path, 0, STATX_BASIC_STATS, extended_status) == 0 &&
                        S_ISCHR(extended_status->stx_mode);
        struct open_how *how = (struct open_how *)(soft0 + 512);
        *how = (struct open_how){.flags = O_RDONLY};
        int openat2_fd = (int)syscall(SYS_openat2, AT_FDCWD, filesystem_path, how, sizeof(*how));
        filesystem_ok = filesystem_ok && openat2_fd >= 0;
        if (openat2_fd >= 0) close(openat2_fd);
        memcpy(filesystem_path, "/tmp", 5);
        int directory_fd = (int)syscall(SYS_openat, AT_FDCWD, filesystem_path, O_RDONLY | O_DIRECTORY, 0);
        long directory_bytes = directory_fd >= 0 ? syscall(SYS_getdents64, directory_fd, soft1 + 1024, 1024) : -1;
        filesystem_ok = filesystem_ok && directory_fd >= 0 && directory_bytes >= 0;
        if (directory_fd >= 0) close(directory_fd);
        struct statfs *filesystem_geometry = (struct statfs *)(soft0 + 2048);
        filesystem_ok = filesystem_ok && syscall(SYS_fstatfs, filesystem_fd, filesystem_geometry) == 0 &&
                        filesystem_geometry->f_bsize > 0;
        int ioctl_pipe[2] = {-1, -1};
        int *available = (int *)(soft1 + 2048);
        char ioctl_byte = 'i';
        filesystem_ok = filesystem_ok && pipe(ioctl_pipe) == 0 &&
                        write(ioctl_pipe[1], &ioctl_byte, 1) == 1 &&
                        syscall(SYS_ioctl, ioctl_pipe[0], FIONREAD, available) == 0 && *available == 1;
        if (ioctl_pipe[0] >= 0) close(ioctl_pipe[0]);
        if (ioctl_pipe[1] >= 0) close(ioctl_pipe[1]);
        if (filesystem_fd >= 0) close(filesystem_fd);

        int tcp_listener = socket(AF_INET, SOCK_STREAM, 0);
        struct sockaddr_in tcp_bound = {.sin_family = AF_INET, .sin_addr.s_addr = htonl(INADDR_LOOPBACK)};
        socklen_t tcp_bound_length = sizeof(tcp_bound);
        *udp_destination = tcp_bound;
        int accept_ok = tcp_listener >= 0 &&
                        bind(tcp_listener, (struct sockaddr *)udp_destination, sizeof(*udp_destination)) == 0 &&
                        listen(tcp_listener, 4) == 0 &&
                        getsockname(tcp_listener, (struct sockaddr *)&tcp_bound, &tcp_bound_length) == 0;
        *udp_destination = tcp_bound;
        int tcp_client = socket(AF_INET, SOCK_STREAM, 0);
        accept_ok = accept_ok && tcp_client >= 0 &&
                    connect(tcp_client, (struct sockaddr *)udp_destination, sizeof(*udp_destination)) == 0;
        *udp_source_length = sizeof(*udp_source);
        int accepted =
            accept_ok ? accept4(tcp_listener, (struct sockaddr *)udp_source, udp_source_length, SOCK_CLOEXEC) : -1;
        accept_ok = accept_ok && accepted >= 0 && udp_source->sin_family == AF_INET;
        if (accepted >= 0) close(accepted);
        if (tcp_client >= 0) close(tcp_client);

        tcp_client = socket(AF_INET, SOCK_STREAM, 0);
        accept_ok = accept_ok && tcp_client >= 0 &&
                    connect(tcp_client, (struct sockaddr *)udp_destination, sizeof(*udp_destination)) == 0;
        errno = 0;
        int bad_length = accept4(tcp_listener, (struct sockaddr *)udp_source, (socklen_t *)direct, 0);
        accept_ok = accept_ok && bad_length < 0 && errno == EFAULT;
        if (tcp_client >= 0) close(tcp_client);

        tcp_client = socket(AF_INET, SOCK_STREAM, 0);
        accept_ok = accept_ok && tcp_client >= 0 &&
                    connect(tcp_client, (struct sockaddr *)udp_destination, sizeof(*udp_destination)) == 0;
        *udp_source_length = sizeof(*udp_source);
        errno = 0;
        int second_span =
            accept4(tcp_listener, (struct sockaddr *)(soft0 + page - 8), udp_source_length, SOCK_NONBLOCK);
        accept_ok = accept_ok && second_span < 0 && errno == EFAULT;
        if (tcp_client >= 0) close(tcp_client);
        if (tcp_listener >= 0) close(tcp_listener);
        if (signal_fd >= 0) close(signal_fd);
        if (timer_fd >= 0) close(timer_fd);
        if (udp_receiver >= 0) close(udp_receiver);
        if (udp_sender >= 0) close(udp_sender);
        event_ok = ctl_ok && write_ok && epoll_ok && ppoll_ok && pselect_ok && signal_fd_ok && timer_fd_ok && udp_ok &&
                   message_ok && mmsg_ok && netlink_ok && aio_ok && sysv_ok && semaphore_ok && shared_ok && queue_ok &&
                   accept_ok && proc_ok && filesystem_ok;
        close(event_pipe[0]);
        close(event_pipe[1]);
    }
    if (ep >= 0) close(ep);

    struct utsname *identity = (struct utsname *)(soft0 + 3000);
    struct sysinfo *system = (struct sysinfo *)(soft1 + 3000);
    unsigned char *random = soft0 + 3400;
    uint32_t *action_available = (uint32_t *)(soft1 + 3400);
    uint16_t *notification_sizes = (uint16_t *)(soft0 + 3440);
    char *hostname = (char *)(soft1 + 3480);
    memcpy(hostname, "logical", 7);
    *action_available = SECCOMP_RET_ALLOW;
    int misc_ok = syscall(SYS_sethostname, hostname, 7) == 0 && syscall(SYS_uname, identity) == 0 &&
                  strcmp(identity->nodename, "logical") == 0 && syscall(SYS_sysinfo, system) == 0 &&
                  system->totalram > 0 && syscall(SYS_getrandom, random, 32, 0) == 32 &&
                  syscall(SYS_seccomp, SECCOMP_GET_ACTION_AVAIL, 0, action_available) == 0 &&
                  syscall(SYS_seccomp, SECCOMP_GET_NOTIF_SIZES, 0, notification_sizes) == 0 &&
                  notification_sizes[2] == 64;
    struct sock_filter *filter = (struct sock_filter *)(soft1 + 3600);
    struct sock_fprog *program = (struct sock_fprog *)(soft0 + 3600);
    filter[0] = (struct sock_filter)BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW);
    *program = (struct sock_fprog){.len = 1, .filter = filter};
    misc_ok = misc_ok && prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0 &&
              syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, program) == 0;

    unsigned *cpu_number = (unsigned *)(soft0 + 3760);
    unsigned *numa_node = (unsigned *)(soft1 + 3760);
    int *schedule_priority = (int *)(soft0 + 3776);
    struct timespec *round_robin = (struct timespec *)(soft1 + 3776);
    unsigned char *schedule_attr = soft0 + 3808;
    struct rlimit *limit = (struct rlimit *)(soft1 + 3808);
    struct itimerval *timer_value = (struct itimerval *)(soft0 + 3888);
    void **move_pages = (void **)(soft1 + 3888);
    int *move_status = (int *)(soft1 + 3920);
    move_pages[0] = soft0;
    move_pages[1] = direct;
    int rare_ok = syscall(SYS_getcpu, cpu_number, numa_node, NULL) == 0 && *numa_node == 0 &&
                  syscall(SYS_sched_getparam, 0, schedule_priority) == 0 && *schedule_priority == 0 &&
                  syscall(SYS_sched_rr_get_interval, 0, round_robin) == 0 && round_robin->tv_nsec == 100000000L &&
                  syscall(SYS_sched_getattr, 0, schedule_attr, 48, 0) == 0 &&
                  *(uint32_t *)schedule_attr == 48 && getrlimit(RLIMIT_NOFILE, limit) == 0 &&
                  limit->rlim_cur > 0 && getitimer(ITIMER_REAL, timer_value) == 0 &&
                  syscall(SYS_move_pages, 0, 2, move_pages, NULL, move_status, 0) == 0 &&
                  move_status[0] == 0 && move_status[1] == -ENOENT;

    printf("syscall-logical-uaccess cross=%d aliases=%d second-page-fault=%d zero-then-fault=%d time=%d signal=%d "
           "event=%d misc-seccomp=%d rare=%d\n",
           cross_ok, aliases_ok, fault_ok, zero_fault_ok, time_ok, signal_ok, event_ok, misc_ok, rare_ok);
    return cross_ok && aliases_ok && fault_ok && zero_fault_ok && time_ok && signal_ok && event_ok && misc_ok &&
                   rare_ok
               ? 0
               : 1;
}
