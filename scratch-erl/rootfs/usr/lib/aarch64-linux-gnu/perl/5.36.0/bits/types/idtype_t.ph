require '_h2ph_pre.ph';

no warnings qw(redefine misc);

unless(defined(&__idtype_t_defined)) {
    eval 'sub __idtype_t_defined () {1;}' unless defined(&__idtype_t_defined);
    eval("sub P_ALL () { 0; }") unless defined(&P_ALL);
    eval("sub P_PID () { 1; }") unless defined(&P_PID);
    eval("sub P_PGID () { 2; }") unless defined(&P_PGID);
    eval("sub P_PIDFD () { 3; }") unless defined(&P_PIDFD);
}
1;
