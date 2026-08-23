static uint64_t ckpt_hash_bytes(uint64_t hash, const void *data, size_t size) {
    const unsigned char *bytes = data;
    for (size_t index = 0; index < size; ++index) {
        hash ^= bytes[index];
        hash *= UINT64_C(1099511628211);
    }
    return hash;
}

static int ckpt_name_compare(const void *left, const void *right) {
    return strcmp(*(const char *const *)left, *(const char *const *)right);
}

static uint64_t ckpt_hash_object(uint64_t hash, const char *name, uint64_t size, const void *data, size_t length) {
    hash = ckpt_hash_bytes(hash, name, strlen(name) + 1);
    hash = ckpt_hash_bytes(hash, &size, sizeof size);
    return ckpt_hash_bytes(hash, data, length);
}

static uint64_t ckpt_hash_combine(uint64_t image, const char *name, uint64_t object) {
    image = ckpt_hash_bytes(image, name, strlen(name) + 1);
    return ckpt_hash_bytes(image, &object, sizeof object);
}
