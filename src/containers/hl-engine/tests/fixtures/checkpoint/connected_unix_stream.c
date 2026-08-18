/* Minimal PostgreSQL-shaped checkpoint socket: a server process owns a bound
 * AF_UNIX listener and an accepted connected stream while its child owns the
 * client endpoint. The accepted endpoint is deliberately installed as fd 10,
 * matching the first unsupported descriptor in the PostgreSQL acceptance log. */
#include <errno.h>
#include <fcntl.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int publish(const char *path, const char *text) {
    int descriptor = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    size_t length = strlen(text);
    int failed = descriptor < 0 || write(descriptor, text, length) != (ssize_t)length;
    if (descriptor >= 0 && close(descriptor) != 0) failed = 1;
    return failed ? -1 : 0;
}

static int client(const char *socket_path, const char *finish) {
    int descriptor = socket(AF_UNIX, SOCK_STREAM, 0);
    if (descriptor < 0) return 20;
    struct sockaddr_un address;
    memset(&address, 0, sizeof address);
    address.sun_family = AF_UNIX;
    size_t length = strlen(socket_path);
    if (length >= sizeof address.sun_path) return 21;
    memcpy(address.sun_path, socket_path, length + 1);
    if (connect(descriptor, (struct sockaddr *)&address,
                (socklen_t)(offsetof(struct sockaddr_un, sun_path) + length + 1)) != 0)
        return 22;
    if (write(descriptor, "BEFORE", 6) != 6) return 23;
    struct timespec pause = {.tv_nsec = 2000000};
    while (access(finish, F_OK) != 0) {
        if (errno != ENOENT || (nanosleep(&pause, NULL) != 0 && errno != EINTR)) return 24;
    }
    if (write(descriptor, "AFTER", 5) != 5) return 25;
    return close(descriptor) == 0 ? 0 : 26;
}

int main(int argc, char **argv) {
    if (argc != 5) return 2;
    int guard = open(argv[3], O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (guard < 0) {
        if (errno == EEXIST) (void)publish(argv[4], "FRESH-START-FALLBACK\n");
        return 90;
    }
    if (close(guard) != 0) return 91;
    char socket_path[sizeof(((struct sockaddr_un *)0)->sun_path)];
    if (snprintf(socket_path, sizeof socket_path, "%s.sock", argv[1]) >= (int)sizeof socket_path) return 3;
    unlink(socket_path);
    int listener = socket(AF_UNIX, SOCK_STREAM, 0);
    if (listener < 0) return 4;
    struct sockaddr_un address;
    memset(&address, 0, sizeof address);
    address.sun_family = AF_UNIX;
    size_t length = strlen(socket_path);
    memcpy(address.sun_path, socket_path, length + 1);
    if (bind(listener, (struct sockaddr *)&address,
             (socklen_t)(offsetof(struct sockaddr_un, sun_path) + length + 1)) != 0 ||
        listen(listener, 1) != 0)
        return 5;

    pid_t child = fork();
    if (child < 0) return 6;
    if (child == 0) {
        close(listener);
        return client(socket_path, argv[2]);
    }
    int connection = accept(listener, NULL, NULL);
    if (connection < 0 || dup2(connection, 10) != 10) return 7;
    if (connection != 10) close(connection);
    char message[6];
    if (read(10, message, sizeof message) != (ssize_t)sizeof message || memcmp(message, "BEFORE", 6) != 0)
        return 8;
    if (publish(argv[1], "READY fd=10 connected=1\n") != 0) return 9;
    char after[5];
    if (read(10, after, sizeof after) != (ssize_t)sizeof after || memcmp(after, "AFTER", 5) != 0) return 10;
    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) return 11;
    if (close(10) != 0 || close(listener) != 0 || unlink(socket_path) != 0) return 12;
    return publish(argv[1], "DONE fd=10 connected=1\n") == 0 ? 0 : 13;
}
