#ifndef HL_LINUX_ABI_HOST_DIRENT_H
#define HL_LINUX_ABI_HOST_DIRENT_H

/*
 * <dirent.h> for this layer.  Same construction and the same REAL/SHAPE/REFUSAL
 * labelling as host_mman.h and host_poll.h.
 *
 * Windows is the awkward case here, and not for the usual reason.  mingw-w64
 * DOES ship a <dirent.h> with a working opendir/readdir/closedir over
 * FindFirstFileW, so the temptation is to use it.  It is refused for one
 * measured reason: its `struct dirent` has no d_type.  Every caller in this
 * layer that walks a directory reads d_type -- the /proc emptiness scan, the
 * overlay whiteout and opaque-dir scans, and getdents64, which must PUT a
 * d_type in the guest's buffer because the Linux structure has one.  A host
 * dirent without the field cannot answer, and the fallback every such caller
 * would need (an lstat per entry) is a different implementation, not a smaller
 * one.
 *
 * Adopting mingw's struct and also defining DT_* would compile and then be
 * wrong in the worst available way: `e->d_type` would not exist, so the code
 * would not compile at all -- or, if the field were papered over, every entry
 * would report the same type.  Defining this layer's own Linux-shaped structure
 * keeps the shape honest and makes the absence a refusal at the call rather
 * than a lie in the data.
 *
 * The Windows implementation therefore uses a Linux-shaped public entry and a
 * FindFirstFileW/FindNextFileW walk, with d_type derived from
 * dwFileAttributes (DIRECTORY -> DT_DIR, REPARSE_POINT with a symlink tag ->
 * DT_LNK, otherwise DT_REG), and d_ino from the file index. fdopendir resolves
 * a CRT descriptor through its Win32 handle and transfers ownership as POSIX
 * requires.
 */

#if !defined(_WIN32)

#include <dirent.h>

#else /* Windows */

#include <errno.h>
#include <io.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <windows.h>

/* SHAPE.  Linux d_type values.  They are handed straight into the guest's
 * dirent64 record by getdents64, so they are the guest's numbers by
 * construction and must not be a host encoding. */
#define DT_UNKNOWN 0
#define DT_FIFO 1
#define DT_CHR 2
#define DT_DIR 4
#define DT_BLK 6
#define DT_REG 8
#define DT_LNK 10
#define DT_SOCK 12
#define DT_WHT 14

/* SHAPE.  The Linux layout, member for member.  d_name is 256 bytes there and
 * this layer copies into it with sizeof, so shortening it would silently
 * truncate names rather than fail. */
struct dirent {
    uint64_t d_ino;
    int64_t d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[256];
};

typedef struct hl_linux_host_dir {
    HANDLE search;
    WIN32_FIND_DATAW found;
    wchar_t directory[PATH_MAX];
    wchar_t pattern[PATH_MAX];
    long position;
    int descriptor;
    struct dirent entry;
} DIR;

static inline uint64_t hl_dirent_inode(const DIR *directory, const wchar_t *name) {
    wchar_t path[PATH_MAX];
    BY_HANDLE_FILE_INFORMATION information;
    size_t base = wcslen(directory->directory), leaf = wcslen(name);
    HANDLE handle;
    uint64_t inode = 0;
    if (base + leaf + 2 > PATH_MAX) return 0;
    memcpy(path, directory->directory, base * sizeof(*path));
    if (base != 0 && path[base - 1] != L'/' && path[base - 1] != L'\\') path[base++] = L'\\';
    memcpy(path + base, name, (leaf + 1) * sizeof(*path));
    handle = CreateFileW(path, FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, NULL,
                         OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT, NULL);
    if (handle != INVALID_HANDLE_VALUE) {
        if (GetFileInformationByHandle(handle, &information))
            inode = ((uint64_t)information.nFileIndexHigh << 32) | information.nFileIndexLow;
        CloseHandle(handle);
    }
    return inode;
}

static inline int hl_dirent_utf8_to_wide(const char *path, wchar_t *wide, size_t capacity) {
    int count;
    if (!path || capacity > INT_MAX) return -1;
    count = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, wide, (int)capacity);
    if (count == 0) {
        errno = EINVAL;
        return -1;
    }
    return 0;
}

static inline DIR *hl_dirent_open_wide(const wchar_t *path, int descriptor) {
    DWORD attributes = GetFileAttributesW(path);
    size_t length;
    DIR *directory;
    if (attributes == INVALID_FILE_ATTRIBUTES || !(attributes & FILE_ATTRIBUTE_DIRECTORY)) {
        errno = ENOTDIR;
        return NULL;
    }
    directory = calloc(1, sizeof(*directory));
    if (!directory) return NULL;
    length = wcslen(path);
    if (length + 3 > PATH_MAX) {
        free(directory);
        errno = ENAMETOOLONG;
        return NULL;
    }
    memcpy(directory->directory, path, (length + 1) * sizeof(*path));
    memcpy(directory->pattern, path, length * sizeof(*path));
    if (length != 0 && path[length - 1] != L'/' && path[length - 1] != L'\\') directory->pattern[length++] = L'\\';
    directory->pattern[length++] = L'*';
    directory->pattern[length] = L'\0';
    directory->search = INVALID_HANDLE_VALUE;
    directory->descriptor = descriptor;
    if (descriptor < 0) {
        HANDLE handle = CreateFileW(path, FILE_LIST_DIRECTORY, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                                    NULL, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, NULL);
        if (handle == INVALID_HANDLE_VALUE ||
            (directory->descriptor = _open_osfhandle((intptr_t)handle, _O_RDONLY)) < 0) {
            if (handle != INVALID_HANDLE_VALUE) CloseHandle(handle);
            free(directory);
            errno = EACCES;
            return NULL;
        }
    }
    return directory;
}

static inline DIR *opendir(const char *path) {
    wchar_t wide[PATH_MAX];
    return hl_dirent_utf8_to_wide(path, wide, PATH_MAX) == 0 ? hl_dirent_open_wide(wide, -1) : NULL;
}

static inline DIR *fdopendir(int descriptor) {
    intptr_t handle = _get_osfhandle(descriptor);
    wchar_t path[PATH_MAX];
    DWORD length;
    if (handle == -1) {
        errno = EBADF;
        return NULL;
    }
    length = GetFinalPathNameByHandleW((HANDLE)handle, path, PATH_MAX, FILE_NAME_NORMALIZED);
    if (length == 0 || length >= PATH_MAX) {
        errno = ENAMETOOLONG;
        return NULL;
    }
    return hl_dirent_open_wide(path, descriptor);
}

static inline struct dirent *readdir(DIR *directory) {
    WIN32_FIND_DATAW *found;
    int length;
    if (!directory) {
        errno = EINVAL;
        return NULL;
    }
    if (directory->search == INVALID_HANDLE_VALUE) {
        directory->search = FindFirstFileW(directory->pattern, &directory->found);
        if (directory->search == INVALID_HANDLE_VALUE) {
            if (GetLastError() == ERROR_FILE_NOT_FOUND) errno = 0;
            else errno = EIO;
            return NULL;
        }
    } else if (!FindNextFileW(directory->search, &directory->found)) {
        if (GetLastError() == ERROR_NO_MORE_FILES) errno = 0;
        else errno = EIO;
        return NULL;
    }
    found = &directory->found;
    memset(&directory->entry, 0, sizeof(directory->entry));
    length = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, found->cFileName, -1, directory->entry.d_name,
                                 (int)sizeof(directory->entry.d_name), NULL, NULL);
    if (length == 0) {
        errno = ENAMETOOLONG;
        return NULL;
    }
    directory->entry.d_off = ++directory->position;
    directory->entry.d_reclen = (unsigned short)(offsetof(struct dirent, d_name) + (size_t)length);
    if ((found->dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) && found->dwReserved0 == IO_REPARSE_TAG_SYMLINK)
        directory->entry.d_type = DT_LNK;
    else if (found->dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY)
        directory->entry.d_type = DT_DIR;
    else
        directory->entry.d_type = DT_REG;
    directory->entry.d_ino = hl_dirent_inode(directory, found->cFileName);
    return &directory->entry;
}

static inline int closedir(DIR *directory) {
    int failed = 0;
    if (!directory) {
        errno = EINVAL;
        return -1;
    }
    if (directory->search != INVALID_HANDLE_VALUE && !FindClose(directory->search)) failed = -1;
    if (_close(directory->descriptor) != 0) failed = -1;
    free(directory);
    return failed;
}

static inline void rewinddir(DIR *directory) {
    if (!directory) return;
    if (directory->search != INVALID_HANDLE_VALUE) FindClose(directory->search);
    directory->search = INVALID_HANDLE_VALUE;
    directory->position = 0;
}

static inline long telldir(DIR *directory) {
    if (!directory) {
        errno = EINVAL;
        return -1;
    }
    return directory->position;
}

static inline void seekdir(DIR *directory, long position) {
    if (!directory || position < 0) return;
    rewinddir(directory);
    while (directory->position < position && readdir(directory)) {}
}

static inline int dirfd(DIR *directory) {
    if (!directory) {
        errno = EINVAL;
        return -1;
    }
    return directory->descriptor;
}

#endif /* _WIN32 */

#endif
