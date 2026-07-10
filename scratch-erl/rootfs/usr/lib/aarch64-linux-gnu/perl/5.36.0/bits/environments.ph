require '_h2ph_pre.ph';

no warnings qw(redefine misc);

unless(defined(&_UNISTD_H)) {
    die("Never include this file directly.  Use <unistd.h> instead");
}
require 'bits/wordsize.ph';
if((defined(&__WORDSIZE) ? &__WORDSIZE : undef) == 64) {
    eval 'sub _POSIX_V7_ILP32_OFF32 () {-1;}' unless defined(&_POSIX_V7_ILP32_OFF32);
    eval 'sub _POSIX_V7_ILP32_OFFBIG () {-1;}' unless defined(&_POSIX_V7_ILP32_OFFBIG);
    eval 'sub _POSIX_V6_ILP32_OFF32 () {-1;}' unless defined(&_POSIX_V6_ILP32_OFF32);
    eval 'sub _POSIX_V6_ILP32_OFFBIG () {-1;}' unless defined(&_POSIX_V6_ILP32_OFFBIG);
    eval 'sub _XBS5_ILP32_OFF32 () {-1;}' unless defined(&_XBS5_ILP32_OFF32);
    eval 'sub _XBS5_ILP32_OFFBIG () {-1;}' unless defined(&_XBS5_ILP32_OFFBIG);
    eval 'sub _POSIX_V7_LPBIG_OFFBIG () {-1;}' unless defined(&_POSIX_V7_LPBIG_OFFBIG);
    eval 'sub _POSIX_V6_LPBIG_OFFBIG () {-1;}' unless defined(&_POSIX_V6_LPBIG_OFFBIG);
    eval 'sub _XBS5_LPBIG_OFFBIG () {-1;}' unless defined(&_XBS5_LPBIG_OFFBIG);
    eval 'sub _POSIX_V7_LP64_OFF64 () {1;}' unless defined(&_POSIX_V7_LP64_OFF64);
    eval 'sub _POSIX_V6_LP64_OFF64 () {1;}' unless defined(&_POSIX_V6_LP64_OFF64);
    eval 'sub _XBS5_LP64_OFF64 () {1;}' unless defined(&_XBS5_LP64_OFF64);
} else {
    eval 'sub _POSIX_V7_ILP32_OFF32 () {1;}' unless defined(&_POSIX_V7_ILP32_OFF32);
    eval 'sub _POSIX_V7_ILP32_OFFBIG () {1;}' unless defined(&_POSIX_V7_ILP32_OFFBIG);
    eval 'sub _POSIX_V6_ILP32_OFF32 () {1;}' unless defined(&_POSIX_V6_ILP32_OFF32);
    eval 'sub _POSIX_V6_ILP32_OFFBIG () {1;}' unless defined(&_POSIX_V6_ILP32_OFFBIG);
    eval 'sub _XBS5_ILP32_OFF32 () {1;}' unless defined(&_XBS5_ILP32_OFF32);
    eval 'sub _XBS5_ILP32_OFFBIG () {1;}' unless defined(&_XBS5_ILP32_OFFBIG);
    eval 'sub _POSIX_V7_LP64_OFF64 () {-1;}' unless defined(&_POSIX_V7_LP64_OFF64);
    eval 'sub _POSIX_V7_LPBIG_OFFBIG () {-1;}' unless defined(&_POSIX_V7_LPBIG_OFFBIG);
    eval 'sub _POSIX_V6_LP64_OFF64 () {-1;}' unless defined(&_POSIX_V6_LP64_OFF64);
    eval 'sub _POSIX_V6_LPBIG_OFFBIG () {-1;}' unless defined(&_POSIX_V6_LPBIG_OFFBIG);
    eval 'sub _XBS5_LP64_OFF64 () {-1;}' unless defined(&_XBS5_LP64_OFF64);
    eval 'sub _XBS5_LPBIG_OFFBIG () {-1;}' unless defined(&_XBS5_LPBIG_OFFBIG);
    eval 'sub __ILP32_OFFBIG_CFLAGS () {"-D_LARGEFILE_SOURCE -D_FILE_OFFSET_BITS=64";}' unless defined(&__ILP32_OFFBIG_CFLAGS);
}
1;
