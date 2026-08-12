# x86-64 Linux guest frontend

Decode x86-64, model architectural flags/registers and lower to the private validated IR. Linux syscall-number
normalization is an architecture hook into `linux_abi`, not a host syscall call.
