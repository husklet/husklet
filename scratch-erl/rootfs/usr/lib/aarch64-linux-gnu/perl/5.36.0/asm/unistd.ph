require '_h2ph_pre.ph';

no warnings qw(redefine misc);

eval 'sub __ARCH_WANT_RENAMEAT () {1;}' unless defined(&__ARCH_WANT_RENAMEAT);
eval 'sub __ARCH_WANT_NEW_STAT () {1;}' unless defined(&__ARCH_WANT_NEW_STAT);
eval 'sub __ARCH_WANT_SET_GET_RLIMIT () {1;}' unless defined(&__ARCH_WANT_SET_GET_RLIMIT);
eval 'sub __ARCH_WANT_TIME32_SYSCALLS () {1;}' unless defined(&__ARCH_WANT_TIME32_SYSCALLS);
eval 'sub __ARCH_WANT_SYS_CLONE3 () {1;}' unless defined(&__ARCH_WANT_SYS_CLONE3);
eval 'sub __ARCH_WANT_MEMFD_SECRET () {1;}' unless defined(&__ARCH_WANT_MEMFD_SECRET);
require 'asm-generic/unistd.ph';
1;
