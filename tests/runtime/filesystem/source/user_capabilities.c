// Capabilities derived from the requested launch user, the check that only became expressible once
// container launch stopped force-setting the container set regardless of uid. Docker grants the
// container set to a root container and nothing to a `--user` one, because runc changes user before
// raising capabilities; the bounding set is the container set either way. Verified on this host:
// `docker run --user 1000 alpine` reports CapPrm/CapEff 0 with CapBnd 00000000a80425fb, and the
// same image without `--user` reports CapPrm/CapEff 00000000a80425fb.
//
// One source, two cases. At uid 0 every denial column reads 0, which is the proof a container that
// never drops root is unaffected. At uid 1000 the ordinary columns must still read 1 -- that is the
// apt/gosu shape a prior attempt broke by dropping capabilities before ownership projection was
// correct -- while the denial columns flip to 1.
//
// `distinct` is the non-vacuity guard. The two identities here are the launch uid and root, who owns
// the image content; a run that silently stayed root would satisfy every ordinary column and every
// denial column would go vacuous, so the guard asserts the live uid really is the requested one and
// really differs from the uid owning the files the denials are measured against.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define OTHER_UID 2000

// A denial counts only when it is the specific errno Linux gives for the missing capability, so a
// path that fails for an unrelated reason cannot be read as enforcement.
static unsigned denied(int result, int wanted) { return result != 0 && errno == wanted; }

// The same rule for `open`, whose failure is a negative descriptor rather than a non-zero status.
// Verified against this Linux host at an unprivileged uid: reading a 0640 root:shadow file, creating
// in root-owned 0755 /etc, and O_WRONLY on a root-owned 0644 file are each EACCES, and so is
// O_RDONLY|O_TRUNC on that file -- O_TRUNC demands write access whatever the access mode asks for.
static unsigned denied_open(const char *path, int flags, mode_t mode, int wanted) {
    int fd = open(path, flags, mode);
    if (fd >= 0) {
        close(fd);
        return 0;
    }
    return errno == wanted;
}

// The capability sets as /proc/self/status reports them. The bounding set has no behavioural
// consequence for this task, so only reading it back can pin it against the Docker contract.
static unsigned long long cap_line(const char *name) {
    FILE *status = fopen("/proc/self/status", "r");
    if (!status) return ~0ULL;
    char line[512];
    unsigned long long value = ~0ULL;
    size_t width = strlen(name);
    while (fgets(line, sizeof line, status)) {
        if (strncmp(line, name, width) == 0) {
            value = strtoull(line + width, NULL, 16);
            break;
        }
    }
    fclose(status);
    return value;
}

int main(void) {
    unsigned live_uid = (unsigned)getuid();
    struct stat status;

    // The image content the denials are measured against, and the identity the guard compares to.
    unsigned etc_uid = lstat("/etc", &status) == 0 ? status.st_uid : 12345;
    unsigned etc_mode = status.st_mode & 07777;
    // The unreadable-to-others file the read denial is measured against, stat'd up front so the
    // guard can prove it really is root-owned and really is 0640 rather than assuming the image.
    unsigned shadow_uid = lstat("/etc/shadow", &status) == 0 ? status.st_uid : 12345;
    unsigned shadow_mode = status.st_mode & 07777;

    // Ordinary work: a directory of its own, a file in it, and the two mkdir shapes a dropped
    // privilege task must keep. Explicit modes, because the umask would otherwise decide this.
    char directory[] = "/tmp/hl-usercaps-XXXXXX";
    if (!mkdtemp(directory)) return 1;
    if (chmod(directory, 0755) != 0) return 2;
    if (chdir(directory) != 0) return 3;

    unsigned made_dir = mkdir("created", 0755) == 0;
    int fd = open("created/file", O_CREAT | O_RDWR, 0644);
    unsigned made_file = fd >= 0;
    if (fd >= 0) close(fd);
    // mkdir inside a directory this task created itself. Reported host uid rather than guest uid
    // here is what produced EACCES the last time capabilities were projected.
    unsigned mkdir_created = mkdir("created/inside", 0755) == 0;
    // chown to the uid already held is the one chown an unprivileged owner may perform, and mkdir
    // inside the result is the second half of the same shape.
    unsigned made_owned = mkdir("owned", 0755) == 0;
    unsigned chown_self = chown("owned", live_uid, (unsigned)getgid()) == 0;
    unsigned mkdir_chowned = mkdir("owned/inside", 0755) == 0;
    // Image content stays readable; a `--user` container that cannot read its own image is broken.
    fd = open("/etc/alpine-release", O_RDONLY);
    unsigned image_read = fd >= 0;
    if (fd >= 0) close(fd);
    // The write and truncate modes against a file this task owns. A permission check on `open` that
    // mapped the access bits too broadly would deny these, so they are the counterweight to the
    // denial columns below.
    fd = open("created/file", O_RDWR);
    unsigned own_rdwr = fd >= 0;
    if (fd >= 0) close(fd);
    fd = open("created/file", O_WRONLY | O_TRUNC);
    unsigned own_trunc = fd >= 0;
    if (fd >= 0) close(fd);

    // What CAP_DAC_OVERRIDE and CAP_CHOWN were papering over. /etc is root-owned 0755, so a non-root
    // task may search and read it but never write into it.
    unsigned etc_mkdir_denied = denied(mkdir("/etc/hl-usercaps", 0755), EACCES);
    // Giving a file away needs CAP_CHOWN even when the task owns it.
    unsigned chown_other_denied = denied(chown("created/file", OTHER_UID, OTHER_UID), EPERM);

    // The same policy reached through `open` rather than through a mutation syscall. Creating needs
    // write on the parent, O_RDONLY needs read on the file, and O_WRONLY and O_TRUNC each need write
    // -- including O_RDONLY|O_TRUNC, which asks for no write in its access mode and needs one anyway.
    unsigned etc_create_denied = denied_open("/etc/hl-usercaps-file", O_CREAT | O_WRONLY, 0644, EACCES);
    unsigned shadow_read_denied = denied_open("/etc/shadow", O_RDONLY, 0, EACCES);
    unsigned etc_write_denied = denied_open("/etc/alpine-release", O_WRONLY, 0, EACCES);
    unsigned etc_trunc_denied = denied_open("/etc/alpine-release", O_RDONLY | O_TRUNC, 0, EACCES);

    // Root holds the container set, so it must clear every denial above rather than be exempted
    // from them. Undo the mutations root is allowed to make, so the columns stay comparable.
    if (live_uid == 0) {
        rmdir("/etc/hl-usercaps");
        unlink("/etc/hl-usercaps-file");
    }

    // Non-vacuity: the task really is the requested uid, and the image content the denials are
    // measured against really belongs to a different uid with a mode that permits the read but not
    // the write. Without this a run that stayed root would satisfy the ordinary columns silently.
    // The read denial is measured against a 0640 root-owned file, so the guard pins that mode too:
    // were it 0644 the denial column would go vacuous by reading as permitted for the wrong reason.
    unsigned distinct = live_uid == (unsigned)geteuid() && etc_uid == 0 && etc_mode == 0755
                        && shadow_uid == 0 && shadow_mode == 0640
                        && (live_uid == 0 || (live_uid != etc_uid && live_uid != shadow_uid));

    printf(
        "user-caps uid=%u distinct=%u prm=%llx eff=%llx bnd=%llx inh=%llx amb=%llx dir=%u file=%u "
        "mkdir_created=%u chown_self=%u owned=%u mkdir_chowned=%u image_read=%u own_rdwr=%u "
        "own_trunc=%u etc_mkdir=%u chown_other=%u etc_create=%u shadow_read=%u etc_write=%u "
        "etc_trunc=%u\n",
        live_uid,
        distinct,
        cap_line("CapPrm:"),
        cap_line("CapEff:"),
        cap_line("CapBnd:"),
        cap_line("CapInh:"),
        cap_line("CapAmb:"),
        made_dir,
        made_file,
        mkdir_created,
        chown_self,
        made_owned,
        mkdir_chowned,
        image_read,
        own_rdwr,
        own_trunc,
        etc_mkdir_denied,
        chown_other_denied,
        etc_create_denied,
        shadow_read_denied,
        etc_write_denied,
        etc_trunc_denied);
    return 0;
}
