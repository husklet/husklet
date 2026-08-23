// ETXTBSY covers the whole exec image set, not only the file named in execve: Linux refuses the
// exec when the ELF program interpreter (PT_INTERP -- the dynamic loader) is held open for writing,
// exactly as it does for the main image.
//
// Measured on the bare host kernel before this case was written, with a dynamically linked victim
// linked against a private copy of the loader so the copy could be held open:
//   interpreter held O_WRONLY by the exec'ing process   -> ETXTBSY
//   interpreter held O_WRONLY by a sibling process      -> ETXTBSY
//   interpreter held O_RDONLY                           -> exec succeeds
//   main image held O_WRONLY                            -> ETXTBSY
// The kernel is the oracle here; none of this was inferred from the engine.
//
// The engine checks the main image and the program interpreter in one /proc/self/fd enumeration.
// Nothing else in the corpus covers the interpreter arm, so dropping it would be silent: this case
// exists to redden when the interpreter stops being checked while the main image still is.
//
// The victim is the image's own dynamically linked busybox rather than a compiled artifact: this
// case must stay statically linked itself, so that its own execution never depends on -- and can
// never be perturbed by -- the loader it holds open.
#define _GNU_SOURCE
#include <elf.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

static char interpreter[PATH_MAX];

// Read the victim's PT_INTERP rather than spelling a loader path: the loader is named per ISA and
// per libc, and the assertion is the same one everywhere.
static int read_program_interpreter(const char *image) {
    int descriptor = open(image, O_RDONLY);
    if (descriptor < 0) return 0;
    Elf64_Ehdr header;
    int ok = read(descriptor, &header, sizeof header) == (ssize_t)sizeof header &&
             memcmp(header.e_ident, ELFMAG, SELFMAG) == 0 && header.e_ident[EI_CLASS] == ELFCLASS64 &&
             header.e_phentsize == (Elf64_Half)sizeof(Elf64_Phdr);
    int found = 0;
    for (Elf64_Half index = 0; ok && !found && index < header.e_phnum; ++index) {
        Elf64_Phdr program;
        off_t offset = (off_t)header.e_phoff + (off_t)index * (off_t)sizeof program;
        if (pread(descriptor, &program, sizeof program, offset) != (ssize_t)sizeof program) break;
        if (program.p_type != PT_INTERP) continue;
        if (program.p_filesz == 0 || program.p_filesz > sizeof interpreter) break;
        found =
            pread(descriptor, interpreter, program.p_filesz, (off_t)program.p_offset) == (ssize_t)program.p_filesz &&
            interpreter[program.p_filesz - 1] == 0 && interpreter[0] != 0;
    }
    close(descriptor);
    return found;
}

// The exec attempt reports the victim's own exit status on success and the refused errno otherwise.
static int exec_holding(const char *victim, const char *held, int mode) {
    pid_t child = fork();
    if (child == 0) {
        if (held != NULL && open(held, mode) < 0) _exit(120);
        char *arguments[] = {(char *)victim, (char *)"true", NULL};
        execve(victim, arguments, environ);
        _exit(errno);
    }
    if (child < 0) return -1;
    int status = 0;
    if (waitpid(child, &status, 0) != child) return -1;
    return WIFEXITED(status) ? WEXITSTATUS(status) : -1;
}

int main(int argc, char **argv) {
    const char *victim = argc > 1 ? argv[1] : "/bin/busybox";
    int found = read_program_interpreter(victim);
    int interpreter_busy = found && exec_holding(victim, interpreter, O_WRONLY) == ETXTBSY;
    int interpreter_shared = found && exec_holding(victim, interpreter, O_RDONLY) == 0;
    int image_busy = exec_holding(victim, victim, O_WRONLY) == ETXTBSY;
    int unheld = exec_holding(victim, NULL, O_RDONLY) == 0;
    printf("interpreter=%d interpreter_busy=%d interpreter_shared=%d image_busy=%d unheld=%d\n", found,
           interpreter_busy, interpreter_shared, image_busy, unheld);
    return !(found && interpreter_busy && interpreter_shared && image_busy && unheld);
}
