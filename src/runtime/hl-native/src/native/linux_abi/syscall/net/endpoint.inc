    case 198: {
        if ((int)a0 == LX_AF_NETLINK) {                     // socket(AF_NETLINK,...) -> socketpair-backed netlink
            G_RET(c) = (uint64_t)nl_open((int)a1, (int)a2); // -host_errno on fail -> svc_done_host translates
            break;
        }
        int ty = (int)a1;
        // Linux rejects a type carrying bits outside SOCK_TYPE_MASK(0xf) other than SOCK_CLOEXEC(0x80000)
        // and SOCK_NONBLOCK(0x800) with EINVAL, before consulting the family. Validate here so a junk-bit
        // type does not silently mask down to a valid type (host would either accept it or return the wrong
        // ESOCKTNOSUPPORT for a masked-but-unsupported value).
        if (ty & ~(0xf | 0x80000 | 0x800)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        int icmp_ty = ty & 0xf;
        int is_icmp =
            (int)a0 == AF_INET && (int)a2 == IPPROTO_ICMP && ((icmp_ty == SOCK_DGRAM) || (icmp_ty == SOCK_RAW));
        // The unprivileged ICMP *datagram* ping socket (`ping localhost` healthcheck) is synthesized from an
        // AF_UNIX socket and its loopback (127/8) echo reflected locally (see icmp_try_send). Gating only on
        // br_on() left the no-bridge / loopback-only case falling through to a real host
        // socket(AF_INET, SOCK_DGRAM, IPPROTO_ICMP), which needs net.ipv4.ping_group_range / CAP_NET_RAW and
        // EACCES's on a locked-down host (e.g. the CI runner's default empty ping_group_range). lo_on() is
        // true whenever the private loopback namespace is active, so synthesize the datagram ping there too.
        // SOCK_RAW ICMP stays gated on br_on() only: a raw socket is privileged, and under loopback-only
        // isolation the host must still enforce EPERM (see the socket-matrix raw_icmp=EPERM oracle).
        int virtual_icmp = is_icmp && (br_on() || (icmp_ty == SOCK_DGRAM && lo_on()));
        // socket (translate Linux domain -> macOS: AF_INET6 10->30, others unchanged). Gate the new fd
        // against the guest's soft RLIMIT_NOFILE -> EMFILE past the cap (the host table is far larger).
        // Bridge ICMP is synthesized below and must not depend on the host's privileged ping-socket policy.
        int r = nofile_gate(virtual_icmp ? socket(AF_UNIX, SOCK_DGRAM, 0) : socket(af_l2m((int)a0), ty & 0xf, (int)a2));
        if (r >= 0) {
            // SIGPIPE suppression: Linux delivers EPIPE (not a fatal signal) to a guest that has
            // SIG_IGN'd SIGPIPE or passes MSG_NOSIGNAL; macOS has no per-call MSG_NOSIGNAL, so make the
            // suppression sticky on the fd. With SO_NOSIGPIPE set at creation, ANY write to a broken
            // socket -- write(2), writev(2), send(2) without MSG_NOSIGNAL -- returns -1/EPIPE instead
            // of raising SIGPIPE. Benign on healthy sockets; only sockets get it, so real pipes/FIFOs
            // keep Linux's default SIGPIPE-on-write semantics.
            (void)hl_native_set_no_sigpipe(r);
            // One spelling on every host: on a host whose descriptor status cannot be queried the two
            // bits live in a side record rather than in fcntl, and the caller must not have to know which.
            hl_linux_socket_apply_type_flags(r, ty);
            if (r < HL_NFD) {
                // AF_INET6 STREAM also gets loopback isolation (::/::1 -> private lo). a0 is the guest's
                // Linux domain value, so test the Linux AF_INET6 (10), not the macOS one (30).
                g_sock_stream[r] = ((ty & 0xf) == SOCK_STREAM && ((int)a0 == AF_INET || (int)a0 == LX_AF_INET6_FAM));
                g_sock_dgram[r] = ((ty & 0xf) == SOCK_DGRAM && ((int)a0 == AF_INET || (int)a0 == LX_AF_INET6_FAM));
                g_udp_local_port[r] = g_udp_peer_port[r] = 0;
                g_udp_local_ip[r] = g_udp_peer_ip[r] = 0;
                g_udp_local_interface[r] = g_udp_peer_interface[r] = 0;
                g_udp_local_v6[r] = g_udp_peer_v6[r] = 0;
                g_sock_seqpacket[r] = 0;
                g_so_error[r] = 0;     // no pending async socket error on a fresh fd
                g_so_reuseport[r] = 0; // SO_REUSEPORT not yet set on a fresh fd
                tcp_shadow_clear(r);   // no shadowed IPPROTO_TCP options on a fresh fd
                ipopt_shadow_clear(r); // ...nor shadowed IPPROTO_IP/IPV6 options
                g_sock_conn[r] = 0;    // fresh socket: not yet connected (see g_sock_conn decl)
                g_sock_connecting[r] = 0;
                g_sock_host_backed[r] = 0;
                g_sock_fam[r] = (uint16_t)a0; // guest address family, for connect/bind EAFNOSUPPORT check
                g_lo_port[r] = 0;
                g_lo_v6[r] = 0;
                g_lo_v6only[r] = 0;
                g_br_port[r] = 0;
                g_br_ip[r] = 0;
                g_br_interface[r] = 0;
                g_tcp_lport[r] = 0;
                g_tcp_listen[r] = 0;
                g_dns_sock[r] = 0;
                g_icmp_kind[r] =
                    ((int)a0 == AF_INET && (int)a2 == 1 && (((ty & 0xf) == SOCK_DGRAM) || ((ty & 0xf) == SOCK_RAW)))
                        ? (uint8_t)(((ty & 0xf) == SOCK_RAW) ? 2 : 1)
                        : 0;
                g_icmp_sock[r] = 0;
                g_icmp_ip[r] = 0;
                g_sock_object[r] = sock_object_new();
                g_sock_peer_object[r] = 0;
            }
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 199: {
        int sv[2];
        // Linux rejects a type carrying bits outside SOCK_TYPE_MASK(0xf) other than SOCK_CLOEXEC(0x80000)
        // and SOCK_NONBLOCK(0x800) with EINVAL, before consulting the family (net/socket.c __sys_socketpair
        // strips the flag bits and validates them exactly like __sys_socket). Mirror the case 198 check so a
        // junk-bit type does not silently mask down to a valid type and wrongly succeed.
        if ((int)a1 & ~(0xf | 0x80000 | 0x800)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // socketpair (translate Linux domain -> macOS). macOS AF_UNIX has no SOCK_SEQPACKET socketpair;
        // emulate it with SOCK_DGRAM, which over a local AF_UNIX pair is reliable, ordered, and preserves
        // message boundaries -- exactly the SEQPACKET guarantees the guest relies on.
        int lty = (int)a1 & 0xf;
        int hty = (lty == SOCK_SEQPACKET) ? SOCK_DGRAM : lty;
        int r = socketpair(af_l2m((int)a0), hty, (int)a2, sv);
        // Either new fd past the guest's soft RLIMIT_NOFILE -> EMFILE; close both so nothing leaks.
        if (r == 0) {
            int cap = guest_nofile_cur();
            if (sv[0] >= cap || sv[1] >= cap) {
                close(sv[0]);
                close(sv[1]);
                G_RET(c) = (uint64_t)(-EMFILE);
                break;
            }
        }
        if (r == 0) {
            // SO_NOSIGPIPE on both ends so a write/send to a peer-closed pair returns EPIPE, never a
            // fatal SIGPIPE (matches Linux EPIPE-to-guest behaviour). See case 198 for the rationale.
            (void)hl_native_set_no_sigpipe(sv[0]);
            (void)hl_native_set_no_sigpipe(sv[1]);
            if ((int)a1 & HL_LINUX_SOCK_CLOEXEC) {
                fcntl(sv[0], F_SETFD, FD_CLOEXEC);
                fcntl(sv[1], F_SETFD, FD_CLOEXEC);
            }
            if ((int)a1 & HL_LINUX_SOCK_NONBLOCK) {
                hl_linux_socket_apply_type_flags(sv[0], HL_LINUX_SOCK_NONBLOCK);
                hl_linux_socket_apply_type_flags(sv[1], HL_LINUX_SOCK_NONBLOCK);
            }
            // The fd pair is written straight into the guest array; a bad/unmapped destination must EFAULT
            // (and leak no fds) like Linux, not fault the engine writing the pair.
            if (guest_accessible_prefix(a3, 2 * sizeof(int), PROT_WRITE) != 2 * sizeof(int)) {
                close(sv[0]);
                close(sv[1]);
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            int guest_pair[2] = {sv[0], sv[1]};
            if (guest_copy_to(a3, guest_pair, sizeof(guest_pair)) != (ssize_t)sizeof(guest_pair)) {
                close(sv[0]);
                close(sv[1]);
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            if ((int)a0 == AF_UNIX) {
                if (sv[0] >= 0 && sv[0] < HL_NFD) {
                    g_sock_fam[sv[0]] = AF_UNIX;
                    g_sock_stream[sv[0]] = (lty == SOCK_STREAM);
                    g_sock_dgram[sv[0]] = (lty == SOCK_DGRAM || lty == SOCK_SEQPACKET);
                    g_sock_pair_peer[sv[0]] = sv[1] + 1;
                    g_sock_peer_pid[sv[0]] = sock_alloc_synth_peer();
                    g_sock_passcred[sv[0]] = 0;
                }
                if (sv[1] >= 0 && sv[1] < HL_NFD) {
                    g_sock_fam[sv[1]] = AF_UNIX;
                    g_sock_stream[sv[1]] = (lty == SOCK_STREAM);
                    g_sock_dgram[sv[1]] = (lty == SOCK_DGRAM || lty == SOCK_SEQPACKET);
                    g_sock_pair_peer[sv[1]] = sv[0] + 1;
                    g_sock_peer_pid[sv[1]] = sock_alloc_synth_peer();
                    g_sock_passcred[sv[1]] = 0;
                }
                sock_pair_identity_assign(sv[0], sv[1]);
            }
            // macOS AF_UNIX has no SEQPACKET, so a SEQPACKET pair is backed by SOCK_DGRAM (above) to keep
            // message boundaries. But a connected DGRAM socket does NOT deliver EOF when its peer closes
            // (a blocked recv never wakes; a fresh recv gets ECONNRESET) -- whereas Linux SEQPACKET recv
            // returns 0 (EOF). Mark both ends so close() injects a zero-length EOF datagram and recv/read
            // translate the peer-closed ECONNRESET to 0. (rustc's jobserver relies on this EOF to exit.)
            if (lty == SOCK_SEQPACKET) {
                // _seqpacket-dgram-maxmsg_: a macOS AF_UNIX DGRAM socket caps SO_SNDBUF at the tiny
                // net.local.dgram.maxdgram default (2048), so ANY send > 2048 bytes fails with EMSGSIZE --
                // whereas a Linux SEQPACKET message is bounded only by SO_SNDBUF (~208KB default) and never
                // hits a 2KB wall. Large SEQPACKET bootstrap messages require the Linux-sized buffer, and bring-up
                // messages carrying serialized handles and initialization IPC routinely exceed
                // 2KB: on the DGRAM backing those sends fail and the message is lost, so the parent/child
                // handshake wedges forever (the UI thread blocks on a readiness event the child can never
                // signal -> no window is ever created). Raise SO_SNDBUF/SO_RCVBUF on BOTH ends (a per-socket
                // buffer overrides the maxdgram default; verified: with 1MB buffers, 256KB datagrams send OK)
                // so an emulated SEQPACKET carries the same large messages a real Linux SEQPACKET does. Both
                // ends are bidirectional (each sends and receives), and the setting survives the fork/exec
                // that hands one end to the child (it is a property of the kernel socket object).
                int bufsz = 1 << 20; // 1 MiB: comfortably above a realistic IPC channel message
                setsockopt(sv[0], SOL_SOCKET, SO_SNDBUF, &bufsz, sizeof bufsz);
                setsockopt(sv[0], SOL_SOCKET, SO_RCVBUF, &bufsz, sizeof bufsz);
                setsockopt(sv[1], SOL_SOCKET, SO_SNDBUF, &bufsz, sizeof bufsz);
                setsockopt(sv[1], SOL_SOCKET, SO_RCVBUF, &bufsz, sizeof bufsz);
                // Stamp each end with a DISTINCT synthetic peer node identity. macOS reports the socketpair
                // CREATOR's pid via LOCAL_PEERPID on BOTH ends (never updated on fork), so once the parent
                // forks a child its cred/peercred query degenerates to self; without a distinct id every
                // forked child collides on guest pid 1 and peer-node merging hangs.
                if (sv[0] >= 0 && sv[0] < HL_NFD) g_sock_seqpacket[sv[0]] = 1;
                if (sv[1] >= 0 && sv[1] < HL_NFD) g_sock_seqpacket[sv[1]] = 1;
                if (seq_ref_pair(sv[0], sv[1]) != 0) {
                    int e = errno;
                    g_sock_seqpacket[sv[0]] = 0;
                    g_sock_seqpacket[sv[1]] = 0;
                    close(sv[0]);
                    close(sv[1]);
                    G_RET(c) = (uint64_t)(-e);
                    break;
                }
            }
            (void)proc_fdvis_publish_native_fd(sv[0]);
            (void)proc_fdvis_publish_native_fd(sv[1]);
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // bind -- port-map: bind the published host port
    case 200: {
        int socket_type = 0;
        socklen_t socket_type_length = sizeof(socket_type);
        if (getsockopt((int)a0, SOL_SOCKET, SO_TYPE, &socket_type, &socket_type_length) < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        void *bind_address __attribute__((cleanup(guest_free_bounce))) = NULL;
        if (a2 > sizeof(struct sockaddr_storage)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        bind_address = malloc(a2 ? (size_t)a2 : 1);
        if (!bind_address) {
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        if (a2 && guest_copy_from(bind_address, a1, (size_t)a2) != (ssize_t)a2) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        a1 = (uint64_t)(uintptr_t)bind_address;
        // Linux errno pre-screen (EBADF/ENOTSOCK/EFAULT/EINVAL/EAFNOSUPPORT) before any addr deref.
        {
            int pc = net_precheck((int)a0, a1, (socklen_t)a2, 0);
            if (pc) {
                G_RET(c) = (uint64_t)(int64_t)pc;
                return svc_done_host(c);
            }
        }
        // GUEST Linux sockaddr_in: family@0(u16 LE), port@2(BE)
        uint8_t *sa = (uint8_t *)a1;
        // Bad address POINTER -> EFAULT, not an engine fault: the loopback/bridge/AF_UNIX classifiers
        // below deref `sa` directly. Validate the declared addrlen (clamped) first. (LTP bind01 EFAULT)
        {
            size_t alc = (size_t)(socklen_t)a2;
            if (alc > sizeof(struct sockaddr_storage)) alc = sizeof(struct sockaddr_storage);
            if (!host_range_mapped((uintptr_t)a1, alc)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
        }
        // remember the guest-requested (addr,port) of a stream socket so a later listen can surface a
        // LISTEN row in /proc/net/tcp[6] (ss/netstat -l). Independent of which network mode the bind resolves
        // to below -- the synthesized table has no real IP stack to read back from. AF is the guest sockaddr
        // family at offset 0 (LE u16); port is BE at offset 2 (identical v4/v6 layout).
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_stream[(int)a0] && a2 >= 8) {
            uint16_t fam = *(uint16_t *)(sa + 0), bp = ntohs(*(uint16_t *)(sa + 2));
            if (fam == AF_INET)
                netns_tcp_bind_note((int)a0, bp, 0, *(uint32_t *)(sa + 4), NULL);
            else if (fam == LX_AF_INET6_FAM && a2 >= 24)
                netns_tcp_bind_note((int)a0, bp, 1, 0, sa + 8);
        }
        // private loopback: v4 127/8 (and 0.0.0.0 in direct mode -- a 0.0.0.0 server must answer 127.0.0.1),
        // or v6 ::1/:: (dual-stack servers bind v6 too; route it to the SAME per-container loopback so it is
        // isolated from the real host stack instead of escaping it). port@2 is identical in v4/v6 layout.
        int is_lo6 = lo6_any_is(sa, (socklen_t)a2);
        if (lo_on() && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_stream[(int)a0] &&
            (lo_any_is(sa, (socklen_t)a2) || is_lo6)) {
            uint16_t p = ntohs(*(uint16_t *)(sa + 2));
            if (p == 0) p = lo_alloc_ephemeral(); // bind(:0) -> a real, round-trippable port
            int v6only = 0;
            if (is_lo6) {
                socklen_t ol = sizeof v6only;
                if (getsockopt((int)a0, IPPROTO_IPV6, IPV6_V6ONLY, &v6only, &ol) != 0) v6only = 0;
            }
            char up[200];
            lo_tcp_path(p, is_lo6 && v6only, up, sizeof up);
            struct sockaddr_un un;
            if (unix_addr_set(&un, up) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if (lo_swap((int)a0) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            // SO_REUSEPORT dual-bind: native Linux lets a second REUSEPORT socket bind the same
            // addr:port. The AF_UNIX switch backs each port by one fs inode, so a second bind would
            // hit EADDRINUSE. Only when THIS socket has SO_REUSEPORT set, drop the existing inode so
            // the rebind replaces it and succeeds. A normal (non-REUSEPORT) rebind keeps the inode and
            // correctly returns EADDRINUSE, which also preserves an active wildcard listener's binding.
            if (g_so_reuseport[(int)a0]) unlink(up);
            int r = bind((int)a0, (struct sockaddr *)&un, sizeof un);
            if (r == 0) {
                g_lo_port[(int)a0] = p ? p : 1;
                g_lo_v6[(int)a0] = (uint8_t)is_lo6; // remember family for getsockname/accept
                g_lo_v6only[(int)a0] = (uint8_t)(is_lo6 && v6only);
                netns_tcp_port_note((int)a0, p); // surface the resolved (ephemeral) port in /proc/net/tcp
                // Own the rendezvous inode with the refcounted-unlink mechanism (shared with UDP): unlike a
                // real INADDR_ANY TCP bind whose port is freed by the kernel on last close, this AF_UNIX
                // inode would otherwise linger on the fs and EADDRINUSE the next bind of the same ip:port
                // (fixed HL_IP -> deterministic path). udp_ref_drop (fd_reset_emul) unlinks it on the LAST
                // reference's close; seq_ref_fork_prepare bumps the ref across fork so a client child that
                // inherits the listener does not orphan it.
                udp_ref_create((int)a0, up);
            }
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        // NET bridge: bind(0.0.0.0 / own-ip / in-subnet :port) -> the namespace's private bridge path.
        // A dual-stack listener that binds `::` (busybox nc's default, and many servers') is the IPv6 analogue
        // of 0.0.0.0 and takes the same path (br6_any_is), so it's reachable by peer containers over the switch
        // instead of landing on the isolated per-container loopback (which broke cross-container reach-by-name).
        int bridge_enabled = br_on();
        int bridge_interface = bridge_enabled ? br_bind_interface(sa, (socklen_t)a2) : -1;
        int bridge_v6_any = bridge_enabled && br6_any_is(sa, (socklen_t)a2);
        int bridge_v4_any = bridge_enabled && sa && a2 >= 8 && *(uint16_t *)sa == AF_INET && *(uint32_t *)(sa + 4) == 0;
        if (bridge_v6_any) bridge_interface = 0;
        if (bridge_enabled && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_stream[(int)a0] && bridge_interface >= 0) {
            uint16_t p = ntohs(*(uint16_t *)(sa + 2));
            if (p == 0 && br_alloc_ephemeral(bridge_interface, &p) != 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            int bridge_v6only = 0;
            if (br6_any_is(sa, (socklen_t)a2)) {
                socklen_t option_length = sizeof bridge_v6only;
                if (getsockopt((int)a0, IPPROTO_IPV6, IPV6_V6ONLY, &bridge_v6only, &option_length) != 0)
                    bridge_v6only = 0;
            }
            char up[200];
            int path_status = bridge_v6only
                                  ? br_v6only_path(bridge_interface, g_netif[bridge_interface].ip, p, up, sizeof up)
                                  : br_path(bridge_interface, g_netif[bridge_interface].ip, p, up, sizeof up);
            if (path_status != 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            struct sockaddr_un un;
            if (unix_addr_set(&un, up) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if (lo_swap((int)a0) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            // SO_REUSEPORT dual-bind over the bridge switch: same rule as the private-loopback path --
            // only a REUSEPORT socket replaces the existing per-port inode; a normal rebind keeps it and
            // returns EADDRINUSE, preserving an active wildcard listener's binding.
            if (g_so_reuseport[(int)a0]) unlink(up);
            int r = bind((int)a0, (struct sockaddr *)&un, sizeof un);
            if (r == 0) {
                g_br_port[(int)a0] = p ? p : 1;
                g_br_ip[(int)a0] = g_netif[bridge_interface].ip;
                g_br_interface[(int)a0] = (uint8_t)(bridge_interface + 1);
                netns_tcp_port_note((int)a0, p); // surface the resolved (ephemeral) port in /proc/net/tcp
                // Own the bridge rendezvous inode with the refcounted-unlink mechanism (shared with UDP) so
                // it is removed on the LAST reference's close instead of lingering to EADDRINUSE the next
                // bind of the same ip:port (see the private-loopback path above for the full rationale).
                if (udp_ref_create((int)a0, up) < 0 ||
                    ((bridge_v4_any || bridge_v6_any) &&
                     br_alias_wildcard_listener((int)a0, bridge_interface, p, bridge_v6only) < 0)) {
                    int saved = errno;
                    udp_ref_drop((int)a0);
                    (void)lo_swap((int)a0); // discard the bound socket after a partial registration
                    r = -1;
                    errno = saved;
                }
            }
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        // abstract AF_UNIX (sun_path[0]==0): macOS has no abstract ns -> bind a real fs socket keyed by
        // HL_NETNS. Must run BEFORE any general AF_UNIX passthrough below.
        if (abs_is(sa, (socklen_t)a2)) {
            char up[200];
            abs_path(sa, (socklen_t)a2, up, sizeof up);
            struct sockaddr_un un;
            if (unix_addr_set(&un, up) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            unlink(up); // replace stale (cf. lo_/br_ above)
            int r = bind((int)a0, (struct sockaddr *)&un, sizeof un);
            if (r == 0) { // record the guest-visible abstract name "@<name>" for /proc/net/unix
                char an[108];
                int L = (int)a2 - 3; // sun_path[0]==0, name follows; addrlen = 2 (family) + 1 (nul) + name
                if (L < 0) L = 0;
                if (L > (int)sizeof an - 2) L = (int)sizeof an - 2;
                an[0] = '@';
                memcpy(an + 1, sa + 3, (size_t)L);
                an[L + 1] = 0;
                unix_bind_note((int)a0, an);
            }
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        // AF_UNIX autobind: bind() with only the family (no name) -> Linux assigns a unique abstract-namespace
        // address. Route it through the same HL_NETNS-keyed fs backing as an explicit abstract bind so the
        // assigned name is both reported by getsockname and reachable by a connecting peer.
        if (sa && a2 >= 2 && *(uint16_t *)sa == AF_UNIX &&
            (socklen_t)a2 <= (socklen_t)offsetof(struct sockaddr_un, sun_path)) {
            static uint32_t g_autobind_seq;
            uint8_t syn[3 + 16];
            *(uint16_t *)syn = AF_UNIX;
            syn[2] = 0; // abstract (leading NUL)
            int nl = snprintf((char *)syn + 3, 13, "%05x",
                              (unsigned)(((uint32_t)getpid() << 12) ^ ++g_autobind_seq) & 0xfffffu);
            char up[200];
            abs_path(syn, (socklen_t)(3 + nl), up, sizeof up);
            struct sockaddr_un un;
            if (unix_addr_set(&un, up) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            unlink(up);
            int r = bind((int)a0, (struct sockaddr *)&un, sizeof un);
            if (r == 0) {
                char an[108];
                an[0] = '@';
                memcpy(an + 1, syn + 3, (size_t)nl);
                an[nl + 1] = 0;
                unix_bind_note((int)a0, an);
            }
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        // AF_UNIX pathname bind: materialize the socket inode in the overlay (writable upper), jail-confined,
        // so the guest can stat/chmod/connect it through the SAME resolver. A raw host bind created the inode
        // OUTSIDE the jail (at the literal guest path on the host fs), so the guest's overlay-routed stat()
        // ENOENT'd it (mongod "Failed to chmod socket file", mariadb "Bind on unix socket"). This also
        // applies to a typed bind volume in bare mode: every pathname operation must select its backing.
        if (unix_path_is(sa, (socklen_t)a2)) {
            char gp[200], host[1024];
            unix_path_copy(sa, (socklen_t)a2, gp, sizeof gp);
            if (!unix_path_routed(gp)) goto bind_passthrough;
            if (g_rootfs)
                overlay_copyup(gp, host, sizeof host); // guest path -> upper host path (+ materialize parent dirs)
            else
                xlate(gp, host, sizeof host); // missing final socket name inside the bare bind volume
            // bind at the FULL upper path (via unix_sock_at, which fchdir-shortens paths past sun_path[104])
            // so the socket inode lands exactly where the guest's stat/chmod/connect resolves -- a plain bind
            // would truncate the long upper path and strand the inode where nothing can find it.
            int r = unix_owner_bind((int)a0, host);
            if (r == 0) unix_bind_note((int)a0, gp); // record the guest path for /proc/net/unix
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
    bind_passthrough:
        // Private UDP uses the same launch-scoped AF_UNIX switch as TCP. This includes ordinary,
        // unpublished loopback and user-network sockets; publishing is an additional host forwarder.
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_dgram[(int)a0]) {
            int ui = br_on() ? br_bind_interface(sa, (socklen_t)a2) : -1;
            int udp_lo6 = lo_on() && lo6_any_is(sa, (socklen_t)a2);
            if (ui >= 0 || (lo_on() && lo_any_is(sa, (socklen_t)a2)) || udp_lo6) {
                uint16_t up = ntohs(*(uint16_t *)(sa + 2));
                uint32_t uip = ui >= 0 ? g_netif[ui].ip : 0;
                int ur = udp_switch_bind((int)a0, ui, uip, up);
                if (ur == 0) g_udp_local_v6[(int)a0] = (uint8_t)udp_lo6;
                if (ur == 0 && up && pm_published(up)) udp_fwd_maybe_start((int)a0);
                G_RET(c) = ur < 0 ? (uint64_t)(-errno) : 0;
                break;
            }
        }
        // Published UDP (`-p H:C/udp`): swap an AF_INET datagram socket bound to a published port onto
        // the AF_UNIX switch + start its host->guest datagram forwarder. No-op (returns 0) for
        // non-published UDP, non-switch nets, or non-datagram sockets -> they fall through unchanged.
        {
            int64_t uret;
            if (udp_bind_maybe((int)a0, sa, (socklen_t)a2, &uret)) {
                G_RET(c) = (uint64_t)uret;
                break;
            }
        }
        if (hl_linux_ports_count(&g_ports) != 0 && sa && a2 >= 8 && *(uint16_t *)(sa + 0) == AF_INET) {
            uint16_t cp = ntohs(*(uint16_t *)(sa + 2)), hp = pm_host(cp);
            // remember for getsockname
            if ((int)a0 >= 0 && (int)a0 < HL_NFD) g_fd_cport[(int)a0] = cp;
            if (hp != cp) {
                uint8_t buf[128];
                socklen_t L = a2 < 128 ? (socklen_t)a2 : 128;
                memcpy(buf, sa, L);
                // publish on :H instead of :C (port @2)
                *(uint16_t *)(buf + 2) = htons(hp);
                if (pm_address(cp) != 0) *(uint32_t *)(buf + 4) = pm_address(cp);
                // Linux->macOS sockaddr translation (sin_len/family) before the real host bind.
                struct sockaddr_storage ss;
                socklen_t hl = sa_l2m(buf, L, &ss);
                int br = (hl != (socklen_t)-1) ? bind((int)a0, (struct sockaddr *)&ss, hl)
                                               : bind((int)a0, (struct sockaddr *)buf, L);
                G_RET(c) = br < 0 ? (uint64_t)(-errno) : 0;
                break;
            }
        }
        // Real host bind: translate Linux AF_INET/INET6 sockaddr -> macOS (sin_len/family); AF_UNIX
        // and others pass through unchanged. (Was: raw bind of the Linux struct -> AF_UNSPEC bind.)
        {
            struct sockaddr_storage ss;
            socklen_t hl = sa_l2m(sa, (socklen_t)a2, &ss);
            int br = (hl != (socklen_t)-1) ? bind((int)a0, (struct sockaddr *)&ss, hl)
                                           : bind((int)a0, (void *)a1, (socklen_t)a2);
            // bare-mode AF_UNIX pathname bind (no overlay jail): record the guest path for /proc/net/unix.
            if (br == 0 && a2 >= 3 && *(uint16_t *)(sa + 0) == AF_UNIX && sa[2]) {
                char gp[108];
                snprintf(gp, sizeof gp, "%.*s", (int)sizeof gp - 1, (const char *)(sa + 2));
                unix_bind_note((int)a0, gp);
            }
            G_RET(c) = br < 0 ? (uint64_t)(-errno) : 0;
        }
        break;
    }
    case 201: {
        int lr = listen((int)a0, (int)a1);
        if (lr == 0 && (int)a0 >= 0 && (int)a0 < HL_NFD) {
            g_tcp_listen[(int)a0] = 1;
            g_sock_backlog[(int)a0] = (int)a1;
        }
        // Published-port (`-p H:C`) host bridge: if this is a switch-backed listen on a published
        // container port, spin up a real host AF_INET listener on :H that relays into the guest.
        if (lr == 0) fwd_maybe_start((int)a0);
        if (lr == 0) netns_tcp_listen_note((int)a0); // arm the /proc/net/tcp[6] LISTEN row

        G_RET(c) = lr < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 202:
    case 242: {
        // accept4 flags: only SOCK_CLOEXEC(0x80000) | SOCK_NONBLOCK(0x800) are defined. Linux
        // (__sys_accept4) rejects any other bit with EINVAL as its FIRST check -- before the listen
        // fd is even looked up (so EINVAL wins over EBADF and over a would-be EAGAIN on a nonblocking
        // listener). Validate here so a junk-bit accept4 doesn't slip through to a real accept().
        if (nr == 242 && ((int)a3 & ~(0x80000 | 0x800))) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        int lfd = (int)a0;
        uint64_t guest_peer = a1, guest_peer_length = a2;
        uint8_t peer_address[sizeof(struct sockaddr_storage)] = {0};
        socklen_t peer_length = 0, peer_capacity = 0;
        // accept / accept4
        int pl = (lfd >= 0 && lfd < HL_NFD) ? g_lo_port[lfd] : 0;
        int pl6 = (lfd >= 0 && lfd < HL_NFD) ? g_lo_v6[lfd] : 0; // listener is AF_INET6 -> report v6 peer
        int pbr = (lfd >= 0 && lfd < HL_NFD) ? g_br_port[lfd] : 0;
        uint32_t pbrip = (lfd >= 0 && lfd < HL_NFD) ? g_br_ip[lfd] : 0;
        // Real host accept writes a macOS sockaddr; receive into a host scratch then translate the
        // peer addr back to Linux layout for the guest. (private-lo / bridge: don't expose unix peer.)
        struct sockaddr_storage hss;
        socklen_t hsl = sizeof hss;
        int want_peer = (!pl && !pbr && a1);
        int r;
        do {
            r = (pl || pbr)
                    ? accept(lfd, NULL, NULL)
                    : accept(lfd, want_peer ? (struct sockaddr *)&hss : NULL, want_peer ? &hsl : (socklen_t *)a2);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        r = nofile_gate(r); // accepted fd past the guest's soft RLIMIT_NOFILE -> EMFILE (host table is larger)
        if (r >= 0) {
            // The peer sockaddr (a1) + addrlen (a2) are written straight into the guest buffer by the
            // writeback below; a bad/unmapped destination must EFAULT like Linux -- consuming the accepted
            // connection but not leaking its fd -- not fault the engine. Validate the addrlen cell then the
            // declared (clamped) sockaddr capacity before any writeback; close the accepted fd on failure.
            if (a1) {
                int bad = !a2 || guest_copy_from(&peer_length, a2, sizeof(peer_length)) != sizeof(peer_length);
                if (!bad) {
                    size_t alc = peer_length;
                    if (alc > sizeof(struct sockaddr_storage)) alc = sizeof(struct sockaddr_storage);
                    if (alc && guest_accessible_prefix(a1, alc, PROT_WRITE) != alc) bad = 1;
                }
                if (bad) {
                    close(r);
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
                peer_capacity = peer_length;
                a1 = (uint64_t)(uintptr_t)peer_address;
                a2 = (uint64_t)(uintptr_t)&peer_length;
            }
            // Accepted connections are sockets too: make SIGPIPE suppression sticky on the new fd so a
            // write/send to a peer that closes returns EPIPE instead of killing the guest (see case 198).
            (void)hl_native_set_no_sigpipe(r);
            if (r >= 0 && r < HL_NFD) {
                g_so_error[r] = 0; // fresh accepted fd carries no pending async error
                g_so_reuseport[r] = 0;
                tcp_shadow_clear(r); // no shadowed IPPROTO_TCP options on a fresh accepted fd
                ipopt_shadow_clear(r);
                g_sock_conn[r] = 1; // an accepted socket is already connected
                g_sock_connecting[r] = 0;
                if (lfd >= 0 && lfd < HL_NFD) g_sock_fam[r] = g_sock_fam[lfd]; // inherit listener's family
                g_sock_object[r] = sock_object_new();
                g_sock_peer_object[r] = 0;
            }
            if ((pl || pbr) && sock_internal_accept_identify(r) < 0) {
                int error = errno;
                close(r);
                G_RET(c) = (uint64_t)(int64_t)(-error);
                break;
            }
            if (nr == 242) { hl_linux_socket_apply_type_flags(r, (int)a3); }
            if (want_peer) {
                socklen_t gcap = a2 ? *(socklen_t *)a2 : 0;
                int ll = sa_m2l((struct sockaddr *)&hss, (uint8_t *)a1, gcap);
                if (ll < 0) ll = sa_un_m2l((struct sockaddr *)&hss, hsl, (uint8_t *)a1, gcap); // AF_UNIX -> Linux
                if (ll < 0) { // other non-inet peer: copy raw host bytes
                    socklen_t n = hsl < gcap ? hsl : gcap;
                    if (gcap) memcpy((void *)a1, &hss, n);
                    if (a2) *(socklen_t *)a2 = hsl;
                } else if (a2)
                    *(socklen_t *)a2 = (socklen_t)ll;
            }
            if (pl) {
                if (r < HL_NFD) {
                    g_lo_port[r] = pl;
                    g_lo_v6[r] = (uint8_t)pl6;
                    g_sock_stream[r] = 1;
                }
                if (pl6)
                    fill_inet6_lo((uint8_t *)a1, (socklen_t *)a2, pl);
                else
                    fill_inet_lo((uint8_t *)a1, (socklen_t *)a2, pl);
            } else if (pbr) {
                if (r < HL_NFD) {
                    g_br_port[r] = pbr;
                    g_br_ip[r] = pbrip;
                    g_sock_stream[r] = 1;
                }
                // peer reported as our virtual listen addr (cf. lo_* simplification)
                fill_inet_br((uint8_t *)a1, (socklen_t *)a2, pbrip, pbr);
            }
            if (guest_peer) {
                size_t copy_length = peer_capacity < peer_length ? peer_capacity : peer_length;
                if (copy_length > sizeof(peer_address)) copy_length = sizeof(peer_address);
                if ((copy_length && guest_copy_to(guest_peer, peer_address, copy_length) != (ssize_t)copy_length) ||
                    guest_copy_to(guest_peer_length, &peer_length, sizeof(peer_length)) != sizeof(peer_length)) {
                    close(r);
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
            }
            // peer = 127.0.0.1:lport
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // connect
    case 203: {
        int socket_type = 0;
        socklen_t socket_type_length = sizeof(socket_type);
        if (getsockopt((int)a0, SOL_SOCKET, SO_TYPE, &socket_type, &socket_type_length) < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        void *connect_address __attribute__((cleanup(guest_free_bounce))) = NULL;
        if (a2 > sizeof(struct sockaddr_storage)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        connect_address = malloc(a2 ? (size_t)a2 : 1);
        if (!connect_address) {
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        if (a2 && guest_copy_from(connect_address, a1, (size_t)a2) != (ssize_t)a2) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        a1 = (uint64_t)(uintptr_t)connect_address;
        // Linux errno pre-screen (EBADF/ENOTSOCK/EFAULT/EINVAL/EISCONN/EAFNOSUPPORT) before any addr deref.
        {
            int pc = net_precheck((int)a0, a1, (socklen_t)a2, 1);
            if (pc) {
                G_RET(c) = (uint64_t)(int64_t)pc;
                return svc_done_host(c);
            }
        }
        // Isolated networking: no external egress (HL_NET_ISOLATE). Loopback is redirected by the lo_* path
        // below; any non-127/8 AF_INET destination is refused, matching docker's null network.
        static int net_isolate = -1;
        if (net_isolate < 0) net_isolate = hl_option_get("HL_NET_ISOLATE") != NULL;
        uint8_t *sa = (uint8_t *)a1;
        // A bad address POINTER must return EFAULT, not fault the engine: the DNS/loopback/AF_UNIX
        // classifiers below deref `sa` directly (Linux copies the sockaddr in before any routing).
        // Validate the declared addrlen (clamped to a real sockaddr) up front. (LTP connect01 EFAULT)
        {
            size_t alc = (size_t)(socklen_t)a2;
            if (alc > sizeof(struct sockaddr_storage)) alc = sizeof(struct sockaddr_storage);
            if (!host_range_mapped((uintptr_t)a1, alc)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
        }
        // Container DNS: connect(127.0.0.11:53) -> swap the socket to a socketpair we answer on (host
        // resolver). Subsequent send/recv on the connected fd are handled by the DNS paths below.
        if (dns_enabled() && dns_dest_is(sa, (socklen_t)a2)) {
            int stream = ((int)a0 >= 0 && (int)a0 < HL_NFD) ? g_sock_stream[(int)a0] : 0;
            if (dns_swap((int)a0, stream) == 0) {
                G_RET(c) = 0;
                break;
            } // swap failed -> fall through to the normal (host loopback) connect
        }
        if (net_isolate && sa && (socklen_t)a2 >= 8 && *(uint16_t *)(sa + 0) == AF_INET &&
            (ntohl(*(uint32_t *)(sa + 4)) >> 24) != 127) {
            G_RET(c) = (uint64_t)(-ENETUNREACH);
            break;
        }
        int c_lo6 = lo6_is(sa, (socklen_t)a2);
        if (lo_on() && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_stream[(int)a0] &&
            // private loopback: v4 127/8 or v6 ::1 (port@2 identical) -> the per-container loopback switch
            (lo_is(sa, (socklen_t)a2) || c_lo6)) {
            uint16_t p = ntohs(*(uint16_t *)(sa + 2));
            char up[200];
            lo_tcp_path(p, c_lo6, up, sizeof up);
            if (c_lo6 && access(up, F_OK) != 0) lo_tcp_path(p, 0, up, sizeof up);
            struct sockaddr_un un;
            if (unix_addr_set(&un, up) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if (lo_swap((int)a0) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if (sock_internal_connect_prepare((int)a0) != 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            int r = connect((int)a0, (struct sockaddr *)&un, sizeof un);
            if (r == 0 || errno == EINPROGRESS) {
                g_sock_connecting[(int)a0] = r < 0;
                g_lo_port[(int)a0] = p ? p : 1;
                g_lo_v6[(int)a0] = (uint8_t)c_lo6;
            } else if ((errno == ENOENT || errno == ECONNREFUSED) && br_on()) {
                // Same-container localhost dial of a server that bound INADDR_ANY on the bridge (br_path,
                // keyed by OUR own IP -- not lo_path): retry there so 127.0.0.1 still reaches a 0.0.0.0
                // listener in bridge mode. The first connect() already POISONED this AF_UNIX socket (a
                // failed BSD connect leaves the fd unusable -- a second connect() on it hangs/EINVALs, which
                // is why a 0.0.0.0 server was unreachable via 127.0.0.1 with a user network attached),
                // so swap in a FRESH AF_UNIX fd before the retry -- exactly as the br_connect loop does.
                char bp[200];
                struct sockaddr_un bu;
                if (br_path(0, g_netif[0].ip, p, bp, sizeof bp) != 0 || unix_addr_set(&bu, bp) < 0 ||
                    lo_swap((int)a0) < 0) {
                    r = -1;
                } else if (sock_internal_connect_prepare((int)a0) != 0) {
                    r = -1;
                } else {
                    r = connect((int)a0, (struct sockaddr *)&bu, sizeof bu);
                    if (r == 0 || errno == EINPROGRESS) {
                        g_sock_connecting[(int)a0] = r < 0;
                        g_br_port[(int)a0] = p ? p : 1;
                        g_br_ip[(int)a0] = g_netif[0].ip;
                        g_br_interface[(int)a0] = 1;
                    }
                }
            }
            // a redirected TCP dial to a port with no listener fails ENOENT (the per-port unix
            // inode doesn't exist); Linux returns ECONNREFUSED for a closed TCP port. Map it (host
            // errno, translated to Linux 111); other errnos including EINPROGRESS pass through.
            if (r < 0) {
                int le = (errno == ENOENT) ? ECONNREFUSED : errno;
                // A non-blocking connect must not surface ECONNREFUSED synchronously: a real TCP stack
                // defers the refusal to the next poll/getsockopt(SO_ERROR). The AF_UNIX switch has no live
                // INET peer to deliver that, so stash the error and report EINPROGRESS -- poll then wakes on
                // the failed AF_UNIX socket and getsockopt(SO_ERROR) hands the ECONNREFUSED back once.
                if (le == ECONNREFUSED && (int)a0 >= 0 && (int)a0 < HL_NFD && (fcntl((int)a0, F_GETFL) & O_NONBLOCK)) {
                    g_so_error[(int)a0] = ECONNREFUSED;
                    G_RET(c) = (uint64_t)(-EINPROGRESS);
                } else {
                    G_RET(c) = (uint64_t)(-le);
                }
            } else {
                G_RET(c) = 0;
                if ((int)a0 >= 0 && (int)a0 < HL_NFD)
                    g_sock_conn[(int)a0] = 1, g_sock_connecting[(int)a0] = 0; // sticky-connected
            }
            break;
        }
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_icmp_kind[(int)a0] && sa && (socklen_t)a2 >= 8 &&
            *(const uint16_t *)sa == AF_INET &&
            (((*(const uint32_t *)(sa + 4)) & 0xffu) == 127u || // loopback 127/8: locally reflected echo
             (br_on() && br_for_ip(*(const uint32_t *)(sa + 4)) >= 0))) {
            g_icmp_ip[(int)a0] = *(const uint32_t *)(sa + 4);
            G_RET(c) = 0;
            break;
        }
        // NET bridge: connect(peer-ip:port in our subnet) -> dial the namespace's private bridge path.
        int bridge_enabled = br_on();
        int connect_interface = bridge_enabled ? br_connect_interface(sa, (socklen_t)a2) : -1;
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_dgram[(int)a0]) {
            // connect(AF_UNSPEC) dissolves a datagram socket's association: clear the recorded peer so a
            // later getpeername reports ENOTCONN, matching the kernel's disconnect idiom.
            if (sa && (socklen_t)a2 >= 2 && *(const uint16_t *)sa == AF_UNSPEC) {
                g_udp_peer_port[(int)a0] = 0;
                g_udp_peer_ip[(int)a0] = 0;
                g_udp_peer_interface[(int)a0] = 0;
                g_udp_peer_v6[(int)a0] = 0;
                g_sock_conn[(int)a0] = 0;
                g_sock_connecting[(int)a0] = 0;
                G_RET(c) = 0;
                break;
            }
            char udp_path[200];
            int udp_interface;
            uint32_t udp_ip;
            uint16_t udp_port;
            int udp_route = udp_switch_destination(sa, (socklen_t)a2, &udp_interface, &udp_ip, &udp_port, udp_path,
                                                   sizeof udp_path);
            if (udp_route < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if (udp_route > 0) {
                if (udp_switch_ensure_source((int)a0, udp_interface) < 0) {
                    G_RET(c) = (uint64_t)(-errno);
                    break;
                }
                g_udp_peer_port[(int)a0] = udp_port;
                g_udp_peer_ip[(int)a0] = udp_ip;
                g_udp_peer_interface[(int)a0] = (uint8_t)(udp_interface + 1);
                g_udp_peer_v6[(int)a0] = (uint8_t)(*(const uint16_t *)sa == LX_AF_INET6_FAM);
                G_RET(c) = 0;
                break;
            }
        }
        if (bridge_enabled && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_stream[(int)a0] && connect_interface >= 0) {
            uint32_t dip = *(uint32_t *)(sa + 4);
            uint16_t p = ntohs(*(uint16_t *)(sa + 2));
            char up[200];
            if (br_path(connect_interface, dip, p, up, sizeof up) != 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            struct sockaddr_un un;
            if (unix_addr_set(&un, up) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            // Retry across a peer's brief re-listen gap: a server looping `nc -l -w N` unbinds+rebinds the
            // switch inode between connections, so a dial that lands in the window sees ENOENT (inode gone)
            // or ECONNREFUSED (stale inode). Recreate the guest fd (lo_swap) + retry for ~600ms, mirroring
            // TCP SYN retransmission; a genuinely-absent peer still fails after the cap. This is what makes
            // a single-shot client (`nc -w 3 <peer> <port>`) reliably reach a `-w 1`-looping listener.
            int r = -1;
            for (int attempt = 0; attempt < 60; attempt++) {
                if (lo_swap((int)a0) < 0) {
                    r = -1;
                    break;
                }
                if (sock_internal_connect_prepare((int)a0) != 0) {
                    r = -1;
                    break;
                }
                r = connect((int)a0, (struct sockaddr *)&un, sizeof un);
                if (r == 0) {
                    // A blocking connect succeeded: verify it isn't a peer mid-exit (a `-w N` listener whose
                    // window just closed accepts nothing and HUPs with no data). If dead-on-arrival, retry a
                    // fresh listener; otherwise it's live (data pending, or a client-first protocol).
                    if (!switch_dead_on_arrival((int)a0)) break;
                } else if (errno == EINPROGRESS) {
                    break; // non-blocking: the guest polls the result itself
                } else if (errno != ENOENT && errno != ECONNREFUSED) {
                    break; // a genuine error -> report it
                }
                r = -1;                             // not connected yet
                errno = ECONNREFUSED;               // if this was the last attempt, report a closed-port error
                struct timespec ts = {0, 20000000}; // 20ms
                nanosleep(&ts, NULL);
            }
            if (r == 0 || errno == EINPROGRESS) {
                g_sock_connecting[(int)a0] = r < 0;
                g_br_port[(int)a0] = p ? p : 1;
                g_br_ip[(int)a0] = dip; // peer ip for getpeername
                g_br_interface[(int)a0] = (uint8_t)(connect_interface + 1);
            }
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            if (r == 0 && (int)a0 >= 0 && (int)a0 < HL_NFD)
                g_sock_conn[(int)a0] = 1, g_sock_connecting[(int)a0] = 0; // sticky-connected
            break;
        }
        // abstract AF_UNIX (sun_path[0]==0): dial the same HL_NETNS-keyed fs socket bind used. Must run
        // BEFORE the general AF_UNIX passthrough below.
        if (abs_is(sa, (socklen_t)a2)) {
            char up[200];
            abs_path(sa, (socklen_t)a2, up, sizeof up);
            struct sockaddr_un un;
            if (unix_addr_set(&un, up) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            int r = connect((int)a0, (struct sockaddr *)&un, sizeof un);
            int ce = errno;
            // An abstract name is not a filesystem object: a missing backing socket means no bound listener,
            // which Linux reports as ECONNREFUSED (the host fs backing yields ENOENT).
            if (r < 0 && ce == ENOENT) ce = ECONNREFUSED;
            G_RET(c) = (r < 0 && ce != EINPROGRESS) ? (uint64_t)(-ce) : 0;
            if ((r == 0 || ce == EINPROGRESS) && (int)a0 >= 0 && (int)a0 < HL_NFD) {
                g_sock_conn[(int)a0] = 1; // sticky-connected
                g_sock_connecting[(int)a0] = r < 0;
                char an[108];
                int L = (int)a2 - 3; // sun_path[0]==0, name follows; addrlen = 2 (family) + 1 (nul) + name
                if (L < 0) L = 0;
                if (L > (int)sizeof an - 2) L = (int)sizeof an - 2;
                an[0] = '@';
                memcpy(an + 1, sa + 3, (size_t)L);
                an[L + 1] = 0;
                unix_peer_note((int)a0, an); // guest-visible peer name for getpeername
            }
            break;
        }
        // AF_UNIX pathname connect: resolve through the overlay (same resolver as stat/open) so we dial the
        // socket the guest actually bound -- materialized in the upper -- not a host path outside the jail.
        if (unix_path_is(sa, (socklen_t)a2)) {
            char gp[200], host[1024];
            unix_path_copy(sa, (socklen_t)a2, gp, sizeof gp);
            if (!unix_path_routed(gp)) goto connect_passthrough;
            const char *hp = atpath(-100, gp, host, sizeof host, 0); // guest path -> topmost layer's host path
            // dial via unix_sock_at (matches the bind side): fchdir-shortens paths past sun_path[104] so a
            // long upper socket path is reached exactly, not truncated to some other (nonexistent) inode.
            int r;
            do {
                r = unix_sock_at((int)a0, hp, 1);
            } while (r < 0 && SVC_EINTR_RESTART(c));
            G_RET(c) = (r < 0 && errno != EINPROGRESS) ? (uint64_t)(-errno) : 0;
            if ((r == 0 || errno == EINPROGRESS) && (int)a0 >= 0 && (int)a0 < HL_NFD) {
                g_sock_conn[(int)a0] = r == 0;
                g_sock_connecting[(int)a0] = r < 0;
                g_sock_native_peer[(int)a0] = (uint8_t)jail_is_projected_socket(gp);
            }
            break;
        }
    connect_passthrough:
        // Real host connect: translate Linux AF_INET/INET6 sockaddr -> macOS; others pass through.
        {
            struct sockaddr_storage ss;
            socklen_t hl = sa_l2m(sa, (socklen_t)a2, &ss);
            // bind(0.0.0.0, ...) on a virtual network swaps a stream onto the AF_UNIX switch so peer
            // containers can reach a listener. Linux also permits that bound socket to connect outward.
            // An external INET sockaddr cannot be passed to the substituted AF_UNIX descriptor
            // (EAFNOSUPPORT), so restore a real INET stream while retaining the guest-visible local bind
            // metadata. Drop the now-defunct switch rendezvous only after replacement succeeds.
            if (hl != (socklen_t)-1 && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_stream[(int)a0] &&
                (g_lo_port[(int)a0] || g_br_port[(int)a0]) && !g_sock_host_backed[(int)a0]) {
                if (inet_stream_swap((int)a0) < 0) {
                    G_RET(c) = (uint64_t)(-errno);
                    break;
                }
                udp_ref_drop((int)a0);
                g_sock_host_backed[(int)a0] = 1;
            }
            // When HL_EGRESS_SOCKS is armed, funnel a genuine external TCP
            // connect through the SOCKS5 proxy instead of dialing directly. INERT when unset — egress_should_
            // redirect() short-circuits to 0, so control falls straight to the direct connect() below with no
            // behavior change. Streams only; UDP/raw and non-INET dests use the direct path.
            if (hl != (socklen_t)-1 && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_stream[(int)a0] &&
                egress_should_redirect((struct sockaddr *)&ss)) {
                int er = egress_connect((int)a0, (struct sockaddr *)&ss, hl);
                G_RET(c) = er < 0 ? (uint64_t)(-errno) : 0;
                if ((er == 0 || errno == EINPROGRESS) && (int)a0 >= 0 && (int)a0 < HL_NFD) {
                    g_sock_conn[(int)a0] = er == 0;
                    g_sock_connecting[(int)a0] = er < 0;
                }
                break;
            }
            // #261 IPv4-only network: a genuine external IPv6 dest has no route -> ENETUNREACH *now* (not a
            // 2-min host-v6 timeout), so happy-eyeballs (apt/curl) falls back to the IPv4 answer immediately.
            if (hl != (socklen_t)-1 && v6_no_route((struct sockaddr *)&ss)) {
                G_RET(c) = (uint64_t)(-ENETUNREACH);
                break;
            }
            int cr;
            do {
                cr = (hl != (socklen_t)-1) ? connect((int)a0, (struct sockaddr *)&ss, hl)
                                           : connect((int)a0, (void *)a1, (socklen_t)a2);
            } while (cr < 0 && SVC_EINTR_RESTART(c));
            G_RET(c) = cr < 0 ? (uint64_t)(-errno) : 0;
            if ((cr == 0 || errno == EINPROGRESS) && (int)a0 >= 0 && (int)a0 < HL_NFD) {
                g_sock_conn[(int)a0] = cr == 0;
                g_sock_connecting[(int)a0] = cr < 0;
            }
        }
        break;
    }
    case 204: {
        // getsockname
        int fd = (int)a0;
        net_sockaddr_copyout address_copyout __attribute__((cleanup(net_sockaddr_copyout_finish)));
        // The addrlen pointer (a2) is dereferenced and the sockaddr buffer (a1) written directly by every
        // branch below; a bad/unmapped pointer must EFAULT like Linux, not fault the engine. Validate the
        // addrlen cell first, then the declared (clamped) sockaddr capacity.
        if (net_sockaddr_copyout_begin(&address_copyout, c, a1, a2) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (address_copyout.active) {
            a1 = (uint64_t)(uintptr_t)address_copyout.address;
            a2 = (uint64_t)(uintptr_t)&address_copyout.length;
        }
        // Abstract-namespace local name: echo the guest name, not the engine's HL_NETNS-keyed backing fs path.
        if (fd >= 0 && fd < HL_NFD && g_unix_bind[fd][0] == '@' && a1) {
            socklen_t gl = 0;
            int ll = unix_name_fill(g_unix_bind[fd], (uint8_t *)a1, a2 ? *(socklen_t *)a2 : 0, &gl);
            if (ll >= 0) {
                if (a2) *(socklen_t *)a2 = gl;
                G_RET(c) = 0;
                break;
            }
        }
        if (fd >= 0 && fd < HL_NFD && g_udp_local_port[fd]) {
            if (g_udp_local_v6[fd])
                fill_inet6_lo((uint8_t *)a1, (socklen_t *)a2, g_udp_local_port[fd]);
            else if (g_udp_local_interface[fd])
                fill_inet_br((uint8_t *)a1, (socklen_t *)a2, g_udp_local_ip[fd], g_udp_local_port[fd]);
            else
                fill_inet_lo((uint8_t *)a1, (socklen_t *)a2, g_udp_local_port[fd]);
            G_RET(c) = 0;
            break;
        }
        if (fd >= 0 && fd < HL_NFD && g_dns_sock[fd]) { // DNS socket: report an AF_INET local addr (0.0.0.0:0)
            if (a1) {
                uint8_t *g = (uint8_t *)a1;
                memset(g, 0, 8);
                *(uint16_t *)g = AF_INET;
                if (a2) *(socklen_t *)a2 = 16;
            }
            G_RET(c) = 0;
            break;
        }
        if (fd >= 0 && fd < HL_NFD && g_lo_port[fd]) {
            if (g_lo_v6[fd])
                fill_inet6_lo((uint8_t *)a1, (socklen_t *)a2, g_lo_port[fd]);
            else
                fill_inet_lo((uint8_t *)a1, (socklen_t *)a2, g_lo_port[fd]);
            G_RET(c) = 0;
            break;
        }
        if (fd >= 0 && fd < HL_NFD && g_br_port[fd]) {
            fill_inet_br((uint8_t *)a1, (socklen_t *)a2, g_br_ip[fd], g_br_port[fd]);
            G_RET(c) = 0;
            break;
        }
        // Real host getsockname returns a macOS sockaddr; receive into host scratch, translate back to
        // Linux layout for the guest (fixes sin_family/sin_len), preserving the portmap port rewrite.
        struct sockaddr_storage hss;
        socklen_t hsl = sizeof hss;
        int r = getsockname(fd, (struct sockaddr *)&hss, &hsl);
        if (r == 0 && a1) {
            socklen_t gcap = a2 ? *(socklen_t *)a2 : 0;
            int ll = sa_m2l((struct sockaddr *)&hss, (uint8_t *)a1, gcap);
            if (ll < 0)
                ll = sa_un_m2l((struct sockaddr *)&hss, hsl, (uint8_t *)a1, gcap); // AF_UNIX -> Linux + guest path
            if (ll < 0) {
                socklen_t n = hsl < gcap ? hsl : gcap;
                if (gcap) memcpy((void *)a1, &hss, n);
                if (a2) *(socklen_t *)a2 = hsl;
            } else {
                if (a2) *(socklen_t *)a2 = (socklen_t)ll;
                if (hl_linux_ports_count(&g_ports) != 0 && fd >= 0 && fd < HL_NFD && g_fd_cport[fd] && gcap >= 4)
                    // app sees the port it asked for (port @2)
                    *(uint16_t *)((uint8_t *)a1 + 2) = htons(g_fd_cport[fd]);
            }
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 205: {
        // getpeername
        int fd = (int)a0;
        net_sockaddr_copyout address_copyout __attribute__((cleanup(net_sockaddr_copyout_finish)));
        // The addrlen pointer (a2) is dereferenced and the sockaddr buffer (a1) written directly by every
        // branch below; a bad/unmapped pointer must EFAULT like Linux, not fault the engine. Validate the
        // addrlen cell first, then the declared (clamped) sockaddr capacity.
        if (net_sockaddr_copyout_begin(&address_copyout, c, a1, a2) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (address_copyout.active) {
            a1 = (uint64_t)(uintptr_t)address_copyout.address;
            a2 = (uint64_t)(uintptr_t)&address_copyout.length;
        }
        // Abstract-namespace peer name: echo the guest name recorded on connect, not the backing fs path.
        if (fd >= 0 && fd < HL_NFD && g_unix_peer[fd][0] == '@' && a1) {
            socklen_t gl = 0;
            int ll = unix_name_fill(g_unix_peer[fd], (uint8_t *)a1, a2 ? *(socklen_t *)a2 : 0, &gl);
            if (ll >= 0) {
                if (a2) *(socklen_t *)a2 = gl;
                G_RET(c) = 0;
                break;
            }
        }
        if (fd >= 0 && fd < HL_NFD && g_udp_peer_port[fd]) {
            if (g_udp_peer_v6[fd])
                fill_inet6_lo((uint8_t *)a1, (socklen_t *)a2, g_udp_peer_port[fd]);
            else if (g_udp_peer_interface[fd])
                fill_inet_br((uint8_t *)a1, (socklen_t *)a2, g_udp_peer_ip[fd], g_udp_peer_port[fd]);
            else
                fill_inet_lo((uint8_t *)a1, (socklen_t *)a2, g_udp_peer_port[fd]);
            G_RET(c) = 0;
            break;
        }
        if (fd >= 0 && fd < HL_NFD && g_dns_sock[fd]) { // DNS socket: peer is the nameserver 127.0.0.11:53
            dns_fill_ns((uint8_t *)a1, (socklen_t *)a2);
            G_RET(c) = 0;
            break;
        }
        if (fd >= 0 && fd < HL_NFD && g_lo_port[fd] && !g_sock_host_backed[fd]) {
            if (g_lo_v6[fd])
                fill_inet6_lo((uint8_t *)a1, (socklen_t *)a2, g_lo_port[fd]);
            else
                fill_inet_lo((uint8_t *)a1, (socklen_t *)a2, g_lo_port[fd]);
            G_RET(c) = 0;
            break;
        }
        if (fd >= 0 && fd < HL_NFD && g_br_port[fd] && !g_sock_host_backed[fd]) {
            fill_inet_br((uint8_t *)a1, (socklen_t *)a2, g_br_ip[fd], g_br_port[fd]);
            G_RET(c) = 0;
            break;
        }
        // Real host getpeername: translate macOS sockaddr back to Linux layout for the guest.
        struct sockaddr_storage hss;
        socklen_t hsl = sizeof hss;
        int r = getpeername(fd, (struct sockaddr *)&hss, &hsl);
        if (r == 0 && a1) {
            socklen_t gcap = a2 ? *(socklen_t *)a2 : 0;
            int ll = sa_m2l((struct sockaddr *)&hss, (uint8_t *)a1, gcap);
            if (ll < 0)
                ll = sa_un_m2l((struct sockaddr *)&hss, hsl, (uint8_t *)a1, gcap); // AF_UNIX -> Linux + guest path
            if (ll < 0) {
                socklen_t n = hsl < gcap ? hsl : gcap;
                if (gcap) memcpy((void *)a1, &hss, n);
                if (a2) *(socklen_t *)a2 = hsl;
            } else if (a2)
                *(socklen_t *)a2 = (socklen_t)ll;
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
