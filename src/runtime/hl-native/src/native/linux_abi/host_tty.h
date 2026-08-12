#ifndef HL_LINUX_ABI_HOST_TTY_H
#define HL_LINUX_ABI_HOST_TTY_H

/*
 * <termios.h> + <sys/ioctl.h> for this layer.
 *
 * Same construction and the same REAL/SHAPE/REFUSAL labelling as host_mman.h:
 * on Linux and macOS this is the two system headers and nothing else, so the
 * preprocessor input on those hosts is exactly what it was before this file
 * existed; on Windows the vocabulary is synthesized here, because the guest
 * ABI's terminal marshalling is compiled unconditionally and there is no
 * <termios.h> on this host at all.
 *
 * WHAT THE CALLERS ACTUALLY DO WITH THIS, which is what decides each label:
 *
 *   - container/netns.c holds a TRANSLATOR between the guest's 36-byte Linux
 *     termios image and the HOST's struct termios.  It reads the flag bits by
 *     name (IGNBRK, OPOST, CS8, ICANON, ...), so those constants are the host's
 *     values, not the guest's.  Giving them the LINUX values here is the same
 *     decision host_mman.h makes for the PROT_ and MAP_ words: it turns that
 *     translation into an identity, which is precisely what makes the Linux
 *     build of the same code correct today.  Nothing is being faked -- the
 *     struct is real memory this layer owns and fills.
 *
 *   - syscall/fs.c, syscall/binding.c and checkpoint.c hold `struct termios`
 *     and `struct winsize` BY VALUE (g_ptm_term[], g_ptm_win[], the checkpoint
 *     manifest replay), copy c_cc with memcpy and take sizeof on it.  So the
 *     type has to be complete and laid out, not opaque.
 *
 * Three kinds of entry, labelled at each one:
 *
 *   REAL     -- genuinely performs the operation.  Everything labelled REAL
 *               below acts only on a caller-owned `struct termios` in this
 *               process's memory (cfmakeraw and the four cf*speed accessors);
 *               POSIX defines those as structure editors that touch no device,
 *               so there is nothing here for a missing host tty to withhold.
 *   SHAPE    -- a type or constant with no behaviour of its own.  Linux values.
 *   REFUSAL  -- errno + -1, never a quiet success.
 *
 * WHY THE DEVICE CALLS ARE REFUSALS, and why a fake success would be worse than
 * usual here.  A Windows console is not a termios device and never becomes one:
 * its mode is a pair of DWORD bitfields read and written with
 * GetConsoleMode/SetConsoleMode, whose bits (ENABLE_LINE_INPUT,
 * ENABLE_ECHO_INPUT, ENABLE_VIRTUAL_TERMINAL_INPUT) cover a strict subset of
 * c_lflag and have no counterpart at all for c_iflag's CR/NL rewriting, for
 * c_cc[VMIN]/c_cc[VTIME], or for line speed.  And there is no pty: the nearest
 * object is a ConPTY pseudoconsole, which is a pair of pipes plus an HPCON, has
 * no ptsname, no /dev/pts index, no controlling-terminal or foreground-process-
 * group binding, and cannot answer TIOCGPGRP/TIOCSCTTY/TIOCGSID at all.
 *
 * ioctl() in particular MUST NOT return 0.  A guest that gets a successful
 * TIOCGWINSZ believes it is on a terminal and sizes its output to whatever
 * happens to be in the struct (an all-zero winsize reads as an 0x0 terminal,
 * which is a different lie, not a smaller one); a successful TIOCSCTTY makes it
 * believe it acquired a controlling terminal it does not have, and it will then
 * expect ^C to arrive as SIGINT.  ENOSYS says "this host cannot do this", which
 * every caller here already has a path for.
 *
 * Three constants below deliberately switch on code that is `#ifdef`-guarded at
 * the call site -- TIOCNOTTY, TIOCGSID and TIOCPKT in syscall/fs.c each have an
 * `#else` arm answering ENOTTY.  Defining them takes the ioctl() arm instead,
 * so the guest sees ENOSYS.  That is the better of the two: ENOTTY asserts the
 * descriptor is not a terminal, which this layer does not know and which may be
 * false, whereas ENOSYS asserts only that the host has no such operation, which
 * is exactly true.
 *
 * NOT here, on purpose:
 *   - isatty().  mingw-w64 already declares it (over the UCRT's _isatty) and it
 *     answers a real question about a real descriptor -- "is this a character
 *     device" -- so a second declaration here would shadow a working call with
 *     a refusal.
 *   - openpty()/forkpty()/login_tty().  Those live in <pty.h>/<util.h>, not in
 *     the two headers this file stands in for, and their only caller
 *     (src/core/activation.c) is a separate translation unit that includes
 *     neither this file nor anything reaching it.  It is a separate port.
 *   - The SIOC* socket ioctl requests.  Nothing in this tree names one; the
 *     socket ioctls it does serve (syscall/fs.c's default arm -> net_ioctl)
 *     arrive as raw guest numbers and are answered from the netns model without
 *     ever passing through a host constant.
 */

#if !defined(_WIN32)

#include <sys/ioctl.h>
#include <termios.h>

#else /* Windows */

#include <errno.h>
#include <stddef.h>
#include <sys/types.h>

/* ---- SHAPE: the termios scalar types.  Linux widths. -------------------- */
typedef unsigned char cc_t;
typedef unsigned int speed_t;
typedef unsigned int tcflag_t;

#define NCCS 32

/*
 * SHAPE.  glibc's userspace struct termios, field for field: four 32-bit flag
 * words, the line-discipline byte, a 32-entry control-character array, and the
 * two speed words glibc appends (the kernel's own struct stops after c_cc; the
 * speeds are what cf*speed below read and write).  checkpoint.c takes
 * `sizeof tio.c_cc` and memcpy()s into it, so the 32 is load-bearing, not
 * decorative.
 */
struct termios {
    tcflag_t c_iflag;
    tcflag_t c_oflag;
    tcflag_t c_cflag;
    tcflag_t c_lflag;
    cc_t c_line;
    cc_t c_cc[NCCS];
    speed_t c_ispeed;
    speed_t c_ospeed;
};

/* ---- SHAPE: c_cc indices.  Linux numbering, including the VSWTC hole at 7
 * that no other host has -- container/netns.c's Linux-index -> host-index table
 * is written against exactly this ordering. ------------------------------- */
#define VINTR 0
#define VQUIT 1
#define VERASE 2
#define VKILL 3
#define VEOF 4
#define VTIME 5
#define VMIN 6
#define VSWTC 7
#define VSTART 8
#define VSTOP 9
#define VSUSP 10
#define VEOL 11
#define VREPRINT 12
#define VDISCARD 13
#define VWERASE 14
#define VLNEXT 15
#define VEOL2 16

/* ---- SHAPE: c_iflag bits.  Linux values. -------------------------------- */
#define IGNBRK 0000001
#define BRKINT 0000002
#define IGNPAR 0000004
#define PARMRK 0000010
#define INPCK 0000020
#define ISTRIP 0000040
#define INLCR 0000100
#define IGNCR 0000200
#define ICRNL 0000400
#define IUCLC 0001000
#define IXON 0002000
#define IXANY 0004000
#define IXOFF 0010000
#define IMAXBEL 0020000
#define IUTF8 0040000

/* ---- SHAPE: c_oflag bits.  Linux values. -------------------------------- */
#define OPOST 0000001
#define OLCUC 0000002
#define ONLCR 0000004
#define OCRNL 0000010
#define ONOCR 0000020
#define ONLRET 0000040
#define OFILL 0000100
#define OFDEL 0000200

/* ---- SHAPE: c_cflag bits and the baud codes.  Linux values. -------------
 * CBAUD (0010017) is the output-speed field and CIBAUD (CBAUD << 16) the input
 * one; CSIZE (0000060) sits between them and overlaps neither, which is why the
 * netns.c translator can mask all three out of one word. */
#define CSIZE 0000060
#define CS5 0000000
#define CS6 0000020
#define CS7 0000040
#define CS8 0000060
#define CSTOPB 0000100
#define CREAD 0000200
#define PARENB 0000400
#define PARODD 0001000
#define HUPCL 0002000
#define CLOCAL 0004000
#define CBAUD 0010017
#define CBAUDEX 0010000
#define CIBAUD 002003600000
#define CMSPAR 010000000000
#define CRTSCTS 020000000000
#define BOTHER 0010000

#define B0 0000000
#define B50 0000001
#define B75 0000002
#define B110 0000003
#define B134 0000004
#define B150 0000005
#define B200 0000006
#define B300 0000007
#define B600 0000010
#define B1200 0000011
#define B1800 0000012
#define B2400 0000013
#define B4800 0000014
#define B9600 0000015
#define B19200 0000016
#define B38400 0000017
#define B57600 0010001
#define B115200 0010002
#define B230400 0010003
#define B460800 0010004
#define B500000 0010005
#define B576000 0010006
#define B921600 0010007
#define B1000000 0010010
#define B1152000 0010011
#define B1500000 0010012
#define B2000000 0010013
#define B2500000 0010014
#define B3000000 0010015
#define B3500000 0010016
#define B4000000 0010017

/* ---- SHAPE: c_lflag bits.  Linux values. -------------------------------- */
#define ISIG 0000001
#define ICANON 0000002
#define XCASE 0000004
#define ECHO 0000010
#define ECHOE 0000020
#define ECHOK 0000040
#define ECHONL 0000100
#define NOFLSH 0000200
#define TOSTOP 0000400
#define ECHOCTL 0001000
#define ECHOPRT 0002000
#define ECHOKE 0004000
#define FLUSHO 0010000
#define PENDIN 0040000
#define IEXTEN 0100000
#define EXTPROC 0200000

/* ---- SHAPE: tcsetattr actions / tcflush queues / tcflow actions. --------
 * syscall/fs.c maps the guest's own numbering onto these BY NAME rather than by
 * value, because on Darwin the two numberings differ; on this host the mapping
 * is an identity and stays correct. */
#define TCSANOW 0
#define TCSADRAIN 1
#define TCSAFLUSH 2

#define TCIFLUSH 0
#define TCOFLUSH 1
#define TCIOFLUSH 2

#define TCOOFF 0
#define TCOON 1
#define TCIOFF 2
#define TCION 3

/* ---- SHAPE: struct winsize.  Identical on every host that has one, and the
 * layout the guest's TIOCGWINSZ/TIOCSWINSZ payload already uses byte for byte,
 * which is why syscall/fs.c casts the guest pointer straight to it. ------- */
struct winsize {
    unsigned short ws_row;
    unsigned short ws_col;
    unsigned short ws_xpixel;
    unsigned short ws_ypixel;
};

/* ---- SHAPE: ioctl request numbers.  Linux values, which on this layer are
 * also the GUEST's request numbers -- syscall/fs.c switches on the guest number
 * as a literal and then re-issues the host call by name, so any divergence
 * between the two would be a silent misroute.  Same identity argument as the
 * flag bits above. ------------------------------------------------------- */
#define TCGETS 0x5401
#define TCSETS 0x5402
#define TCSETSW 0x5403
#define TCSETSF 0x5404
#define TIOCEXCL 0x540C
#define TIOCSCTTY 0x540E
#define TIOCGPGRP 0x540F
#define TIOCSPGRP 0x5410
#define TIOCOUTQ 0x5411
#define TIOCSTI 0x5412
#define TIOCGWINSZ 0x5413
#define TIOCSWINSZ 0x5414
#define TIOCMGET 0x5415
#define TIOCMSET 0x5418
#define TIOCPKT 0x5420
#define FIONBIO 0x5421
#define TIOCNOTTY 0x5422
#define FIONREAD 0x541B
#define TIOCINQ FIONREAD
#define TIOCGSID 0x5429
#define TIOCGPTPEER 0x5441
#define FIONCLEX 0x5450
#define FIOCLEX 0x5451
#define TIOCGPTN 0x80045430
#define TIOCSPTLCK 0x40045431

/*
 * REAL.  POSIX is explicit that cf{get,set}{i,o}speed touch only the structure
 * handed to them -- no device is involved and none of them can fail on a host
 * with no terminals -- so these are the one part of <termios.h> that Windows
 * costs nothing.
 *
 * They store the speed NUMERICALLY in c_ispeed/c_ospeed rather than encoding a
 * Bxxx code into c_cflag's CBAUD field.  That is the BSD/Darwin convention, and
 * it is the one the callers were written against: container/netns.c converts
 * the guest's Linux baud CODE to bits/s with its own table, calls cfsetospeed()
 * with the NUMBER, and on the way back converts cfgetospeed()'s result from a
 * number to a code.  Encoding into CBAUD here would make that round trip lose
 * the rate, because netns.c also rewrites the CBAUD field itself immediately
 * afterwards.  Storing it in the dedicated speed words keeps the round trip
 * exact and leaves c_cflag entirely to the caller.
 */
static inline speed_t cfgetispeed(const struct termios *attributes) {
    return attributes->c_ispeed;
}

static inline speed_t cfgetospeed(const struct termios *attributes) {
    return attributes->c_ospeed;
}

static inline int cfsetispeed(struct termios *attributes, speed_t speed) {
    attributes->c_ispeed = speed;
    return 0;
}

static inline int cfsetospeed(struct termios *attributes, speed_t speed) {
    attributes->c_ospeed = speed;
    return 0;
}

/* REAL.  Also a pure structure editor: exactly the bits glibc's cfmakeraw
 * clears and sets, plus the VMIN=1/VTIME=0 pair. */
static inline void cfmakeraw(struct termios *attributes) {
    attributes->c_iflag &= (tcflag_t) ~(IGNBRK | BRKINT | PARMRK | ISTRIP | INLCR | IGNCR | ICRNL | IXON);
    attributes->c_oflag &= (tcflag_t)~OPOST;
    attributes->c_lflag &= (tcflag_t) ~(ECHO | ECHONL | ICANON | ISIG | IEXTEN);
    attributes->c_cflag &= (tcflag_t) ~(CSIZE | PARENB);
    attributes->c_cflag |= (tcflag_t)CS8;
    attributes->c_cc[VMIN] = 1;
    attributes->c_cc[VTIME] = 0;
}

/*
 * REFUSAL, from here down.  Each of these needs a line discipline behind the
 * descriptor and there is none on this host -- see the header note for why a
 * console mode is not one and a ConPTY is not one either.
 *
 * tcgetattr() is the one worth calling out separately: returning 0 with a
 * zeroed struct would tell the guest it is on a terminal in fully raw, no-echo,
 * B0 mode, and a shell or a readline caller would then never restore the mode
 * it thinks it saved.  ENOSYS instead.
 */
static inline int tcgetattr(int descriptor, struct termios *attributes) {
    (void)descriptor;
    (void)attributes;
    errno = ENOSYS;
    return -1;
}

static inline int tcsetattr(int descriptor, int action, const struct termios *attributes) {
    (void)descriptor;
    (void)action;
    (void)attributes;
    errno = ENOSYS;
    return -1;
}

static inline int tcflush(int descriptor, int queue) {
    (void)descriptor;
    (void)queue;
    errno = ENOSYS;
    return -1;
}

static inline int tcdrain(int descriptor) {
    (void)descriptor;
    errno = ENOSYS;
    return -1;
}

static inline int tcflow(int descriptor, int action) {
    (void)descriptor;
    (void)action;
    errno = ENOSYS;
    return -1;
}

static inline int tcsendbreak(int descriptor, int duration) {
    (void)descriptor;
    (void)duration;
    errno = ENOSYS;
    return -1;
}

/* REFUSAL.  Foreground-process-group ownership of a terminal is a kernel
 * relation Windows does not model at all: no process groups, no sessions, no
 * controlling terminal -- nothing to read and nothing to set.  tcgetpgrp() and
 * tcgetsid() return pid_t, whose failure value is -1. */
static inline pid_t tcgetpgrp(int descriptor) {
    (void)descriptor;
    errno = ENOSYS;
    return (pid_t)-1;
}

static inline int tcsetpgrp(int descriptor, pid_t group) {
    (void)descriptor;
    (void)group;
    errno = ENOSYS;
    return -1;
}

static inline pid_t tcgetsid(int descriptor) {
    (void)descriptor;
    errno = ENOSYS;
    return (pid_t)-1;
}

/*
 * REFUSAL.  The single most important one in this file.
 *
 * Variadic to match every host's declaration, and the third argument is ignored
 * rather than inspected -- there is no request this can serve.  A caller that
 * read a 0 from here would have been told its descriptor is a terminal of some
 * particular size, in some particular mode, owned by some particular process
 * group; all three are claims this host cannot make.
 */
static inline int ioctl(int descriptor, unsigned long request, ...) {
    (void)descriptor;
    (void)request;
    errno = ENOSYS;
    return -1;
}

/*
 * REFUSAL.  The pty family.  posix_openpt() would have to produce a descriptor,
 * and on Windows a ConPTY is an HPCON plus two pipe HANDLEs that no descriptor
 * names -- the same missing descriptor-to-HANDLE table host_mman.h refuses
 * file-backed mmap over.  ptsname() has no answer even in principle: a ConPTY
 * has no device node, so there is no path to return, and handing back a
 * plausible "/dev/pts/N" would send the caller to open a file that does not
 * exist.  NULL with errno set is ptsname(3)'s own failure convention, and
 * syscall/fs.c already reads a NULL as "not a master".
 */
static inline int posix_openpt(int flags) {
    (void)flags;
    errno = ENOSYS;
    return -1;
}

static inline int grantpt(int descriptor) {
    (void)descriptor;
    errno = ENOSYS;
    return -1;
}

static inline int unlockpt(int descriptor) {
    (void)descriptor;
    errno = ENOSYS;
    return -1;
}

static inline char *ptsname(int descriptor) {
    (void)descriptor;
    errno = ENOSYS;
    return NULL;
}

static inline int ptsname_r(int descriptor, char *buffer, size_t capacity) {
    (void)descriptor;
    (void)buffer;
    (void)capacity;
    errno = ENOSYS;
    return ENOSYS; /* the _r form reports through its return value, not errno */
}

#endif /* _WIN32 */

#endif
