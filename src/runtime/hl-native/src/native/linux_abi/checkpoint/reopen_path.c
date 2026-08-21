/* Linux renders an unlinked descriptor as "<path> (deleted)".  A concurrent
 * atomic replacement may already have recreated the pathname, in which case
 * reopening that live path is the only path-backed interpretation available.
 * A genuinely pathless file must fail closed until file-content images are
 * supported; never serialize the procfs annotation as a literal pathname. */
static int ckpt_normalize_reopen_path(char *path) {
    static const char deleted[] = " (deleted)";
    size_t length = strlen(path);
    size_t suffix = sizeof deleted - 1;
    if (length < suffix || strcmp(path + length - suffix, deleted) != 0) return 0;
    path[length - suffix] = '\0';
    return access(path, F_OK) == 0 ? 0 : 1;
}
