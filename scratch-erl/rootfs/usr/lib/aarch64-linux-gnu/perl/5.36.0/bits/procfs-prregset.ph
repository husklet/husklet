require '_h2ph_pre.ph';

no warnings qw(redefine misc);

unless(defined(&_SYS_PROCFS_H)) {
    die("Never include <bits/procfs-prregset.h> directly; use <sys/procfs.h> instead.");
}
1;
