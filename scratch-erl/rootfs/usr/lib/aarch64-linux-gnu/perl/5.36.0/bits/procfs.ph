require '_h2ph_pre.ph';

no warnings qw(redefine misc);

unless(defined(&_SYS_PROCFS_H)) {
    die("Never include <bits/procfs.h> directly; use <sys/procfs.h> instead.");
}
eval 'sub ELF_NGREG () {($sizeof{\'struct user_regs_struct\'} / $sizeof{ &elf_greg_t});}' unless defined(&ELF_NGREG);
1;
