require '_h2ph_pre.ph';

no warnings qw(redefine misc);

unless(defined(&_SYS_PROCFS_H)) {
    eval 'sub _SYS_PROCFS_H () {1;}' unless defined(&_SYS_PROCFS_H);
    require 'features.ph';
    require 'sys/time.ph';
    require 'sys/types.ph';
    require 'sys/user.ph';
    require 'bits/procfs.ph';
    require 'bits/procfs-id.ph';
    eval 'sub ELF_PRARGSZ () {(80);}' unless defined(&ELF_PRARGSZ);
    require 'bits/procfs-prregset.ph';
    require 'bits/procfs-extra.ph';
}
1;
