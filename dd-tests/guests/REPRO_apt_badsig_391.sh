#!/bin/bash
# Deterministic OFFLINE reproduction of #391 (apt-get update BADSIG 871920D1991BC93C under dd).
# Root cause: under dd, the Ubuntu resolute gpgv (GnuPG 2.4.8 / libgcrypt 1.12.0) ALWAYS selects the
# FIRST keyblock of a multi-key keyring instead of searching for the key whose id matches the signature.
# The Ubuntu archive keyring lists the 2012 CD-image key first and the 2018 archive key (871920D1991BC93C)
# last, so dd verifies against the wrong key -> BADSIG. Native and qemu-aarch64 both verify GOOD.
#
# Proven byte-identical to native under dd (NOT the cause): SHA-512 (KAT, all sizes), file read/pread/mmap,
# lseek/position, glibc memcmp, ARM SHA-1/SHA-256 crypto-ext instructions, gpgv on a SINGLE-key ring.
# Bisect: reproduces identically at v0.9.22 and HEAD with pcache + all kill-switches disabled -> not a
# v0.9.22..HEAD source regression on this bench; exposed by resolute's newer gpgv build.
set -e
DDJIT_DIR=${DDJIT_DIR:?set to the dir containing ddjit-linux_aarch64}
UB=/Users/x/OrbStack/ubuntu                       # a resolute rootfs (gpgv 2.4.8 + ubuntu-archive-keyring)
RING="$UB/usr/share/keyrings/ubuntu-archive-keyring.gpg"  # 3 keyblocks: 2012, 2012, 2018(last)
REL="$UB/var/lib/apt/lists/ports.ubuntu.com_ubuntu-ports_dists_resolute_InRelease"
RUN=/Users/x/ddverify; rm -rf "$RUN"; mkdir -p "$RUN/tmp"
cp "$RING" "$RUN/tmp/fullring.gpg"; cp "$REL" "$RUN/tmp/whole"
JIT="$DDJIT_DIR/ddjit-linux_aarch64"
echo "### native (host):";       "$UB/lib/ld-linux-aarch64.so.1" --library-path "$UB/usr/lib/aarch64-linux-gnu:$UB/lib/aarch64-linux-gnu" "$UB/usr/bin/gpgv" --keyring "$RUN/tmp/fullring.gpg" "$RUN/tmp/whole" 2>&1 | grep -i signature
echo "### qemu-aarch64:";        QEMU_LD_PREFIX="$UB" qemu-aarch64 "$UB/usr/bin/gpgv" --keyring "$RUN/tmp/fullring.gpg" "$RUN/tmp/whole" 2>&1 | grep -i signature
echo "### dd (BADSIG here):";    mac bash -lc "exec env HOME=/tmp '$JIT' --rootfs '$RUN' --lower '$UB' /usr/bin/gpgv --status-fd 1 --keyring /tmp/fullring.gpg /tmp/whole" 2>&1 | grep -iE 'CONSIDERED|signature'
