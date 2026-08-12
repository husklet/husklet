#ifndef HL_LINUX_ABI_CHECKPOINT_H
#define HL_LINUX_ABI_CHECKPOINT_H

struct hl_cmsg_kqueue_meta;

int typed_inotify_scm_image_export(struct hl_cmsg_kqueue_meta *metadata, int marker);
int typed_inotify_scm_image_import(int fd, const struct hl_cmsg_kqueue_meta *metadata, int marker);
int epoll_scm_image_export(struct hl_cmsg_kqueue_meta *metadata, int marker);
int epoll_scm_image_import(int fd, const struct hl_cmsg_kqueue_meta *metadata, int marker);

#endif
