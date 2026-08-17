    case 212: {
        net_message_bounce message_bounce __attribute__((cleanup(net_message_bounce_finish)));
        int bounce_error = net_message_bounce_begin(&message_bounce, c, a1, nr == 212);
        if (bounce_error < 0) {
            G_RET(c) = (uint64_t)(int64_t)bounce_error;
            break;
        }
        uint8_t *g = message_bounce.header;
        // The msghdr struct itself is read (iov ptr/count, name, control, flags) AND written back (recvmsg's
        // namelen/controllen/flags) directly through `g`; a NULL or unmapped msghdr pointer would fault the
        // engine on the very first field access below. Validate it up front so a bad pointer yields EFAULT
        // (like the kernel's copy_from_user), matching the sendmmsg/recvmmsg array guard. Linux msghdr is 56 B.
        uint64_t giov_count = *(uint64_t *)(g + 24);
        struct iovec rebased_iov[1024];
        struct iovec *guest_iov = (struct iovec *)net_nonpie_p(*(uint64_t *)(g + 16));
        if (giov_count > 1024 ||
            (giov_count && guest_bad_ptr((uintptr_t)guest_iov, (size_t)giov_count * sizeof *guest_iov))) {
            G_RET(c) = (uint64_t)(giov_count > 1024 ? -EMSGSIZE : -EFAULT);
            break;
        }
        for (uint64_t i = 0; i < giov_count; ++i) {
            rebased_iov[i] = guest_iov[i];
            rebased_iov[i].iov_base = (void *)net_nonpie_p((uint64_t)(uintptr_t)guest_iov[i].iov_base);
        }
        // msg_name is a guest pointer INSIDE the (already-validated) msghdr; the DNS/icmp classifiers and the
        // sockaddr translator below all deref it directly. A wild pointer (e.g. (void*)-1) must yield EFAULT,
        // not fault the engine -- validate it once here, before any of those derefs (dns_dest_is reads 8 B).
        {
            uint8_t *mn = (uint8_t *)net_nonpie_p(*(uint64_t *)(g + 0));
            socklen_t mnl = *(uint32_t *)(g + 8);
            if (mn && mnl && guest_bad_ptr((uintptr_t)mn, mnl)) {
                G_RET(c) = (uint64_t)(int64_t)-EFAULT;
                break;
            }
        }
        // Oversized UDP datagram -> EMSGSIZE, before routing (see udp_dgram_maxlen).
        if (nr == 211) {
            size_t udp_cap = udp_dgram_maxlen((int)a0);
            if (udp_cap) {
                size_t tot = 0;
                for (uint64_t i = 0; i < giov_count; ++i)
                    tot += rebased_iov[i].iov_len;
                if (tot > udp_cap) {
                    G_RET(c) = (uint64_t)(-EMSGSIZE);
                    break;
                }
            }
        }
        // Container DNS: a sendmsg carrying a query to 127.0.0.11:53 (or on an already-swapped DNS socket).
        if (nr == 211 && dns_enabled()) {
            int dfd = (int)a0;
            uint8_t *nm = (uint8_t *)net_nonpie_p(*(uint64_t *)(g + 0));
            socklen_t nml = *(uint32_t *)(g + 8);
            if ((dfd >= 0 && dfd < HL_NFD && g_dns_sock[dfd]) || dns_dest_is(nm, nml)) {
                uint8_t tmp[2048];
                size_t tl = dns_gather(rebased_iov, (int)giov_count, tmp, sizeof tmp);
                int64_t dret;
                if (dns_try_send(dfd, tmp, tl, nm, nml, &dret)) {
                    G_RET(c) = (uint64_t)dret;
                    break;
                }
            }
        }
        if (nr == 211 && (int)a0 >= 0 && (int)a0 < HL_NFD && g_icmp_kind[(int)a0]) {
            uint8_t tmp[2048];
            uint8_t *nm = (uint8_t *)net_nonpie_p(*(uint64_t *)(g + 0));
            socklen_t nml = *(uint32_t *)(g + 8);
            size_t tl = dns_gather(rebased_iov, (int)giov_count, tmp, sizeof tmp);
            int64_t iret;
            if (icmp_try_send((int)a0, tmp, tl, nm, nml, &iret)) {
                G_RET(c) = (uint64_t)iret;
                break;
            }
        }
        struct msghdr mh;
        // Linux: iovlen/controllen are 8-byte; macOS 4
        memset(&mh, 0, sizeof mh);
        mh.msg_name = (void *)net_nonpie_p(*(uint64_t *)(g + 0));
        mh.msg_namelen = *(uint32_t *)(g + 8);
        mh.msg_iov = rebased_iov;
        mh.msg_iovlen = (int)giov_count;
        mh.msg_flags = *(uint32_t *)(g + 48);
        // msg_name sockaddr: Linux<->macOS translation through a host scratch (AF_INET/INET6 only).
        struct sockaddr_storage nss;
        uint8_t *gname = (uint8_t *)mh.msg_name;
        socklen_t gnamelen = mh.msg_namelen;
        char ud_host[1200];
        int ud_route = 0;                     // AF_UNIX pathname/abstract dgram dest -> overlay/abstract route on send
        if (nr == 211 && gname && gnamelen) { // sendmsg: guest -> host
            int udp_interface;
            uint32_t udp_ip;
            uint16_t udp_port;
            int udp_route = (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_dgram[(int)a0]
                                ? udp_switch_destination(gname, gnamelen, &udp_interface, &udp_ip, &udp_port, ud_host,
                                                         sizeof ud_host)
                                : 0;
            if (udp_route < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if (udp_route > 0) {
                if (udp_switch_ensure_source((int)a0, udp_interface) < 0) {
                    G_RET(c) = (uint64_t)(-errno);
                    break;
                }
                ud_route = 1;
            } else if (gnamelen >= 2 && *(const uint16_t *)gname == AF_UNIX &&
                       unix_dgram_dest(gname, gnamelen, ud_host, sizeof ud_host)) {
                ud_route = 1; // sent via unix_dgram_sendmsg_at below (it owns msg_name)
            } else {
                socklen_t hl = sa_l2m(gname, gnamelen, &nss);
                if (hl != (socklen_t)-1) {
                    // #261 IPv4-only network: an external IPv6 dest has no route -> ENETUNREACH now (mirrors
                    // connect()/sendto), so a v6-first datagram client falls back to IPv4 immediately.
                    if (v6_no_route((struct sockaddr *)&nss)) {
                        G_RET(c) = (uint64_t)(-ENETUNREACH);
                        break;
                    }
                    mh.msg_name = &nss;
                    mh.msg_namelen = hl;
                }
            }
        } else if (nr == 211 && !gname && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_dgram[(int)a0]) {
            int udp_route = udp_switch_peer_path((int)a0, ud_host, sizeof ud_host);
            if (udp_route < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            ud_route = udp_route > 0;
        } else if (nr == 212 && gname && gnamelen) { // recvmsg: receive into host scratch
            mh.msg_name = &nss;
            mh.msg_namelen = sizeof nss;
        }
        // Ancillary data: the guest control buf is Linux-cmsg layout; macOS reads a different cmsghdr,
        // so route it through a host-layout scratch buffer (NULL-control left untouched, so edge/msgflags
        // with no control buffer stays on the old path).
        uint8_t *gc = (void *)net_nonpie_p(*(uint64_t *)(g + 32));
        size_t gcl = *(uint64_t *)(g + 40);
        size_t hcap = 0;
        if (gc && gcl) {
            if (gcl > (SIZE_MAX - 256) / 3) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            hcap = CMSG_SPACE(gcl * 3 + 256);
        }
        if (hcap < 4096) hcap = 4096;
        uint8_t hstack[4096];
        uint8_t *hctl = hcap <= sizeof hstack ? hstack : malloc(hcap);
        if (hcap && !hctl) {
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        if (nr == 211) { // sendmsg: translate guest -> host before the call
            // Ancillary data may carry SCM_RIGHTS fds to another process; flush all RAM-backed scratch so a
            // passed fd (and any other) is a coherent host file on the receiving side.
            if (gc && gcl) memf_materialize_all();
            int cerr = 0;
            int engine_metadata = (int)a0 < 0 || (int)a0 >= HL_NFD || !g_sock_native_peer[(int)a0];
            ssize_t hn = (gc && gcl) ? cmsg_l2m(gc, gcl, hctl, hcap, engine_metadata, &cerr) : 0;
            if (hn < 0) {
                cmsg_tmpfds_close();
                cmsg_seq_finish(0);
                cmsg_event_finish(0);
                cmsg_inflight_finish(0);
                if (hctl != hstack) free(hctl);
                G_RET(c) = (uint64_t)(-(cerr ? cerr : EINVAL));
                break;
            }
            mh.msg_control = hn > 0 ? hctl : NULL;
            mh.msg_controllen = hn > 0 ? (socklen_t)hn : 0;
        } else { // recvmsg: receive into host scratch
            if (gc && gcl) memset(hctl, 0, hcap);
            mh.msg_control = (gc && gcl) ? hctl : NULL;
            mh.msg_controllen = (gc && gcl) ? (socklen_t)hcap : 0;
        }
        // MSG_NOSIGNAL(0x4000) -> SO_NOSIGPIPE (macOS has no per-call flag); EPIPE instead of SIGPIPE.
        if (nr == 211 && ((int)a2 & 0x4000)) { (void)hl_native_set_no_sigpipe((int)a0); }
        // Zero-length address-peek idiom (recvmsg): if the guest wants the sender but supplies no receive
        // room, macOS returns 0 at once without filling msg_name (see dgram_addr_peek). Receive into a
        // 1-byte scratch iov so it blocks and reports the source; MSG_PEEK keeps the datagram queued.
        char one;
        struct iovec sciov = {&one, 1};
        int peekaddr = 0;
        if (nr == 212) {
            size_t totlen = 0;
            struct iovec *iv = (struct iovec *)mh.msg_iov;
            for (int i = 0; iv && i < (int)mh.msg_iovlen; i++)
                totlen += iv[i].iov_len;
            if ((peekaddr = dgram_addr_peek((int)a0, gname && gnamelen, totlen))) {
                mh.msg_iov = &sciov;
                mh.msg_iovlen = 1;
            }
        }
        ssize_t r;
        do {
            r = (nr == 211) ? (ud_route ? (ssize_t)unix_dgram_sendmsg_at((int)a0, ud_host, &mh, msgflags_l2m((int)a2))
                                        : sendmsg((int)a0, &mh, msgflags_l2m((int)a2)))
                            : recvmsg((int)a0, &mh, msgflags_l2m((int)a2));
        } while (r < 0 && SVC_EINTR_RESTART(c));
        if (nr == 211) cmsg_tmpfds_close();
        if (nr == 211) cmsg_seq_finish(r >= 0);
        if (nr == 211) cmsg_event_finish(r >= 0);
        if (nr == 211) cmsg_inflight_finish(r >= 0);
        if (r > 0 && peekaddr) r = 0; // guest supplied no data room; only the source address was wanted
        // SEQPACKET-as-DGRAM EOF: coerce a peer-closed recvmsg's ECONNRESET to 0 (EOF). (See case 199.)
        if (nr == 212 && r < 0 && errno == ECONNRESET && seq_is((int)a0)) r = 0;
        if (nr == 212 && r >= 0) {
            // recvmsg writes back name len + (host->guest) control + translated flags
            if (gname && gnamelen && (int)a0 >= 0 && (int)a0 < HL_NFD && g_icmp_sock[(int)a0]) {
                fill_inet_br(gname, NULL, g_icmp_ip[(int)a0], 0);
                *(uint32_t *)(g + 8) = 16;
            } else if (gname && gnamelen && (int)a0 >= 0 && (int)a0 < HL_NFD && g_dns_sock[(int)a0]) {
                // DNS socket: report the nameserver (127.0.0.11:53) as the source (see case 207).
                dns_fill_ns(gname, NULL);
                *(uint32_t *)(g + 8) = 16;
            } else if (gname && gnamelen &&
                       udp_switch_source((int)a0, &nss, mh.msg_namelen, gname, (socklen_t *)(g + 8))) {
                // AF_UNIX switch sender restored to its Linux AF_INET or AF_INET6 identity.
            } else if (gname && gnamelen) { // translate received host sockaddr back to Linux layout
                int ll = sa_m2l((struct sockaddr *)&nss, gname, gnamelen);
                *(uint32_t *)(g + 8) = (ll >= 0) ? (uint32_t)ll : mh.msg_namelen;
                if (ll < 0 && mh.msg_namelen) // non-inet: copy raw host bytes back
                    memcpy(gname, &nss, mh.msg_namelen < gnamelen ? mh.msg_namelen : gnamelen);
            } else
                *(uint32_t *)(g + 8) = mh.msg_namelen;
            // SO_PASSCRED: the Linux kernel auto-attaches an SCM_CREDENTIALS record with the peer's ucred to
            // every received message; macOS does not, so synthesize it (uid/gid = container identity, pid =
            // the peer's -- LOCAL_PEERPID, mapping the container init's host pid back to guest pid 1, self as
            // the container pid). IPC bootstrap may abort with "missing credentials" without it.
            int passcred_active = gc && gcl && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_passcred[(int)a0];
#if defined(__linux__)
            // The Linux kernel attaches the sender's real SCM_CREDENTIALS.  Synthesizing the Darwin
            // fallback here would replace a post-fork sender pid with the socketpair creator's pid.
            int synth_passcred = 0;
#else
            int synth_passcred = passcred_active;
#endif
            int cred_trunc = 0;
            size_t ln = 0;
            if (synth_passcred) {
                int ppid = 0;
#if defined(__APPLE__)
                socklen_t pl = sizeof ppid;
                if (getsockopt((int)a0, SOL_LOCAL, LOCAL_PEERPID, &ppid, &pl) < 0 || ppid <= 0 || ppid == getpid()) {
#else
                struct ucred peer = {0};
                socklen_t pl = sizeof peer;
                if (getsockopt((int)a0, SOL_SOCKET, SO_PEERCRED, &peer, &pl) == 0) ppid = peer.pid;
                if (ppid <= 0 || ppid == getpid()) {
#endif
                    // macOS reports the socketpair CREATOR's pid on both ends (never updated on fork), so the
                    // fork parent reads its OWN pid here for every child -> container_pid() collapsed all of
                    // them to guest 1, colliding peer node identities. Prefer the end's distinct synthetic
                    // peer node id stamped at socketpair(); fall back to this guest's own pid only if unstamped.
                    int sp = ((int)a0 >= 0 && (int)a0 < HL_NFD) ? g_sock_peer_pid[(int)a0] : 0;
                    ppid = sp ? sp : container_pid();
                } else {
                    int guest_peer;
                    if (hl_linux_pidmap_guest_checked(&g_pidmap, ppid, &guest_peer) != 0) {
                        G_RET(c) = (uint64_t)(int64_t)-ESRCH;
                        break;
                    }
                    ppid = (!g_pidmap.active && g_init_hostpid && ppid == g_init_hostpid) ? 1 : guest_peer;
                }
                size_t ln2 = cmsg_add_cred(gc, 0, gcl, ppid, cuid(), cgid());
                if (ln2 == 0)
                    cred_trunc = 1; // no room for the Linux-mandated credentials record
                else
                    ln = ln2;
            }
            int cmsg_trunc = 0;
            if (gc && gcl) ln = (size_t)cmsg_m2l(&mh, gc, gcl, ln, &cmsg_trunc);
            int host_mflags = (int)mh.msg_flags;
            if (!cmsg_trunc && gc && gcl) host_mflags &= ~MSG_CTRUNC; // host-side sideband expansion compressed cleanly
            int mfl = msgflags_m2l(host_mflags);
            if (cred_trunc || (cmsg_trunc && !passcred_active)) mfl |= 0x8;          // MSG_CTRUNC
            if (((int)a2 & 0x40000000) && gc && ln) cmsg_lx_set_cloexec_fds(gc, ln); // MSG_CMSG_CLOEXEC
            *(uint64_t *)(g + 40) = ln;
            *(uint32_t *)(g + 48) = (uint32_t)mfl;
        }
        if (hctl != hstack) free(hctl);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        // sendmsg to a peer-closed socket -> guest SIGPIPE (Linux), unless MSG_NOSIGNAL was requested.
        if (nr == 211 && !((int)a2 & 0x4000)) svc_sigpipe_on_epipe(c, (int64_t)G_RET(c));
        break;
    }
    case 269:
    // sendmmsg/recvmmsg(fd, mmsghdr[], vlen, flags, [timeout])
    case 243: {
        uint8_t *vec = (uint8_t *)a1;
        unsigned vlen = (unsigned)a2;
        if (vlen > 1024) {
            G_RET(c) = (uint64_t)-EINVAL;
            break;
        }
        /*
         * Drive each element through the already-audited single-message path.
         * This preserves Linux's partial-success contract while sharing the
         * canonical msghdr/iovec/name/control staging and ancillary-data
         * translation with sendmsg/recvmsg.
         */
        size_t vector_bytes = (size_t)vlen * 64;
        if (vector_bytes && (guest_accessible_prefix(a1, vector_bytes, PROT_READ) != vector_bytes ||
                             guest_accessible_prefix(a1, vector_bytes, PROT_WRITE) != vector_bytes)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        unsigned completed = 0;
        for (; completed < vlen; ++completed) {
            uint8_t message[64];
            uint64_t message_address = a1 + (size_t)completed * sizeof(message);
            if (guest_copy_from(message, message_address, sizeof(message)) != sizeof(message)) {
                if (!completed) G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            uint64_t nested_flags = a3;
            if (nr == 243 && completed) nested_flags |= 0x40; /* MSG_DONTWAIT / WAITFORONE */
            (void)svc_net(c, nr == 269 ? 211 : 212, a0, (uint64_t)(uintptr_t)message, nested_flags, 0, 0, 0);
            int64_t result = (int64_t)G_RET(c);
            if (result < 0) {
                if (!completed) return 1; /* nested svc_done_host already translated the errno */
                break;
            }
            uint32_t message_length = (uint32_t)result;
            memcpy(message + 56, &message_length, sizeof(message_length));
            if (guest_copy_to(message_address, message, sizeof(message)) != sizeof(message)) {
                if (!completed) G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        if (completed || vlen == 0) G_RET(c) = completed;
        break;
        // The mmsghdr array itself is read AND written (msg_len/msg_namelen) per submessage; a straddling or
        // unmapped vec would fault the engine on the very first field access below. Validate the whole array up
        // front so a bad pointer yields EFAULT (like the kernel's copy_from_user) instead of a guest-crashes-engine
        // SIGSEGV. Each mmsghdr is 64 bytes (msghdr 56 + msg_len 4 + pad).
        if (vlen && guest_bad_ptr((uintptr_t)vec, (size_t)vlen * 64)) {
            G_RET(c) = (uint64_t)(int64_t)-EFAULT;
            break;
        }
        // mmsghdr = msghdr(56) + msg_len(4) + pad
        // Container DNS: glibc's default parallel A+AAAA lookup sends BOTH queries to the nameserver in one
        // sendmmsg. Answer each submessage via the host resolver; the responses are drained by recvfrom (207).
        if (nr == 269 && dns_enabled() && vlen) {
            int dfd = (int)a0;
            uint8_t *g0 = vec;
            uint8_t *nm0 = (uint8_t *)net_nonpie_p(*(uint64_t *)(g0 + 0));
            socklen_t nml0 = *(uint32_t *)(g0 + 8);
            int is_dns = (dfd >= 0 && dfd < HL_NFD && g_dns_sock[dfd]);
            // nm0 is submessage 0's guest msg_name; dns_dest_is derefs it (8 B). A wild pointer must not fault
            // the engine here -- if unreadable, skip DNS classification and let the main loop below return
            // EFAULT for this submessage (its per-iter msg_name guard). See the sendmsg guard above.
            if (nm0 && nml0 && guest_bad_ptr((uintptr_t)nm0, nml0)) {
                nm0 = NULL;
                nml0 = 0;
            }
            if (!is_dns && dns_dest_is(nm0, nml0) &&
                dns_swap(dfd, (dfd >= 0 && dfd < HL_NFD) ? g_sock_stream[dfd] : 0) == 0)
                is_dns = 1;
            if (is_dns) {
                int stream = (dfd >= 0 && dfd < HL_NFD) ? g_sock_stream[dfd] : 0;
                unsigned n;
                for (n = 0; n < vlen; n++) {
                    uint8_t *g = vec + (size_t)n * 64;
                    uint64_t ivn = *(uint64_t *)(g + 24);
                    struct iovec riv[1024];
                    struct iovec *iv = (struct iovec *)net_nonpie_p(*(uint64_t *)(g + 16));
                    if (ivn > 1024 || (ivn && guest_bad_ptr((uintptr_t)iv, (size_t)ivn * sizeof *iv))) {
                        G_RET(c) = (uint64_t)(ivn > 1024 ? -EMSGSIZE : -EFAULT);
                        goto mmsg_done;
                    }
                    for (uint64_t j = 0; j < ivn; ++j) {
                        riv[j] = iv[j];
                        riv[j].iov_base = (void *)net_nonpie_p((uint64_t)(uintptr_t)iv[j].iov_base);
                    }
                    uint8_t tmp[2048];
                    size_t tl = dns_gather(riv, (int)ivn, tmp, sizeof tmp);
                    dns_send(dfd, tmp, tl, stream);
                    *(uint32_t *)(g + 56) = (uint32_t)tl; // msg_len: whole query accepted
                }
                G_RET(c) = (uint64_t)vlen;
                break;
            }
        }
        int done = 0, err = 0;
        // MSG_NOSIGNAL(0x4000) -> SO_NOSIGPIPE once before the fan-out (macOS has no per-call flag).
        if (nr == 269 && ((int)a3 & 0x4000)) { (void)hl_native_set_no_sigpipe((int)a0); }
        for (unsigned i = 0; i < vlen; i++) {
            uint8_t *g = vec + (size_t)i * 64;
            struct msghdr mh;
            uint64_t giov_count = *(uint64_t *)(g + 24);
            struct iovec rebased_iov[1024];
            struct iovec *guest_iov = (struct iovec *)net_nonpie_p(*(uint64_t *)(g + 16));
            if (giov_count > 1024 ||
                (giov_count && guest_bad_ptr((uintptr_t)guest_iov, (size_t)giov_count * sizeof *guest_iov))) {
                err = giov_count > 1024 ? EMSGSIZE : EFAULT;
                break;
            }
            for (uint64_t j = 0; j < giov_count; ++j) {
                rebased_iov[j] = guest_iov[j];
                rebased_iov[j].iov_base = (void *)net_nonpie_p((uint64_t)(uintptr_t)guest_iov[j].iov_base);
            }
            // Oversized UDP datagram -> EMSGSIZE for this submessage (prior sends still count).
            if (nr == 269) {
                size_t udp_cap = udp_dgram_maxlen((int)a0);
                if (udp_cap) {
                    size_t tot = 0;
                    for (uint64_t j = 0; j < giov_count; ++j)
                        tot += rebased_iov[j].iov_len;
                    if (tot > udp_cap) {
                        err = EMSGSIZE;
                        break;
                    }
                }
            }
            memset(&mh, 0, sizeof mh);
            mh.msg_name = (void *)net_nonpie_p(*(uint64_t *)(g + 0));
            mh.msg_namelen = *(uint32_t *)(g + 8);
            mh.msg_iov = rebased_iov;
            mh.msg_iovlen = (int)giov_count;
            mh.msg_flags = *(uint32_t *)(g + 48);
            // msg_name sockaddr: Linux<->macOS translation through a host scratch (AF_INET/INET6 only).
            struct sockaddr_storage nss;
            uint8_t *gname = (uint8_t *)mh.msg_name;
            socklen_t gnamelen = mh.msg_namelen;
            // Wild per-submessage msg_name must yield EFAULT for this submessage, not fault the engine (the
            // family probe / udp_switch_destination / sa_l2m below all deref gname). Mirrors the iov guard.
            if (gname && gnamelen && guest_bad_ptr((uintptr_t)gname, gnamelen)) {
                err = EFAULT;
                break;
            }
            char ud_host[1200];
            int ud_route = 0; // AF_UNIX pathname/abstract dgram dest -> overlay/abstract route on send
            if (nr == 269 && gname && gnamelen) { // sendmmsg: guest -> host
                int udp_interface;
                uint32_t udp_ip;
                uint16_t udp_port;
                int udp_route = (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_dgram[(int)a0]
                                    ? udp_switch_destination(gname, gnamelen, &udp_interface, &udp_ip, &udp_port,
                                                             ud_host, sizeof ud_host)
                                    : 0;
                if (udp_route < 0) {
                    err = errno;
                    break;
                }
                if (udp_route > 0) {
                    // AF_INET(6) dest over the private-loopback switch: resolve to the peer AF_UNIX path
                    // and send there (mirrors sendto/sendmsg cases 206/211).
                    if (udp_switch_ensure_source((int)a0, udp_interface) < 0) {
                        err = errno;
                        break;
                    }
                    ud_route = 1;
                } else if (gnamelen >= 2 && *(const uint16_t *)gname == AF_UNIX &&
                           unix_dgram_dest(gname, gnamelen, ud_host, sizeof ud_host)) {
                    ud_route = 1; // sent via unix_dgram_sendmsg_at below (it owns msg_name)
                } else {
                    socklen_t hl = sa_l2m(gname, gnamelen, &nss);
                    if (hl != (socklen_t)-1) {
                        mh.msg_name = &nss;
                        mh.msg_namelen = hl;
                    }
                }
            } else if (nr == 269 && !gname && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_dgram[(int)a0]) {
                int udp_route = udp_switch_peer_path((int)a0, ud_host, sizeof ud_host);
                if (udp_route < 0) {
                    err = errno;
                    break;
                }
                ud_route = udp_route > 0; // connected UDP over the switch: send to the recorded peer AF_UNIX path
            } else if (nr == 243 && gname && gnamelen) { // recvmmsg: receive into host scratch
                mh.msg_name = &nss;
                mh.msg_namelen = sizeof nss;
            }
            // Ancillary data: route the per-submessage control buf through a host-layout scratch buffer.
            uint8_t *gc = (void *)net_nonpie_p(*(uint64_t *)(g + 32));
            size_t gcl = *(uint64_t *)(g + 40);
            size_t hcap = 0;
            if (gc && gcl) {
                if (gcl > (SIZE_MAX - 256) / 3) {
                    err = ENOMEM;
                    break;
                }
                hcap = CMSG_SPACE(gcl * 3 + 256);
            }
            if (hcap < 4096) hcap = 4096;
            uint8_t hstack[4096];
            uint8_t *hctl = hcap <= sizeof hstack ? hstack : malloc(hcap);
            if (hcap && !hctl) {
                err = ENOMEM;
                break;
            }
            if (nr == 269) { // sendmmsg: translate guest -> host
                int cerr = 0;
                int engine_metadata = (int)a0 < 0 || (int)a0 >= HL_NFD || !g_sock_native_peer[(int)a0];
                ssize_t hn = (gc && gcl) ? cmsg_l2m(gc, gcl, hctl, hcap, engine_metadata, &cerr) : 0;
                if (hn < 0) {
                    cmsg_tmpfds_close();
                    cmsg_seq_finish(0);
                    cmsg_event_finish(0);
                    cmsg_inflight_finish(0);
                    if (hctl != hstack) free(hctl);
                    err = cerr ? cerr : EINVAL;
                    break;
                }
                mh.msg_control = hn > 0 ? hctl : NULL;
                mh.msg_controllen = hn > 0 ? (socklen_t)hn : 0;
            } else { // recvmmsg: receive into host scratch
                if (gc && gcl) memset(hctl, 0, hcap);
                mh.msg_control = (gc && gcl) ? hctl : NULL;
                mh.msg_controllen = (gc && gcl) ? (socklen_t)hcap : 0;
            }
            int rf = (int)a3;
            // after the first, don't block (MSG_WAITFORONE-ish)
            if (nr == 243 && i > 0) rf |= 0x40;
            ssize_t r = (nr == 269)
                            ? (ud_route ? (ssize_t)unix_dgram_sendmsg_at((int)a0, ud_host, &mh, msgflags_l2m(rf))
                                        : sendmsg((int)a0, &mh, msgflags_l2m(rf)))
                            : recvmsg((int)a0, &mh, msgflags_l2m(rf));
            if (nr == 269) cmsg_tmpfds_close();
            if (nr == 269) cmsg_event_finish(r >= 0);
            if (nr == 269) cmsg_inflight_finish(r >= 0);
            if (r < 0) {
                err = errno;
                if (hctl != hstack) free(hctl);
                break;
            }
            // msg_len
            *(uint32_t *)(g + 56) = (uint32_t)r;
            if (nr == 243) {
                if (gname && gnamelen && (int)a0 >= 0 && (int)a0 < HL_NFD && g_dns_sock[(int)a0]) {
                    dns_fill_ns(gname, NULL); // DNS socket: source is the nameserver (see case 207)
                    *(uint32_t *)(g + 8) = 16;
                } else if (gname && gnamelen) { // translate received host sockaddr back to Linux layout
                    int ll = sa_m2l((struct sockaddr *)&nss, gname, gnamelen);
                    *(uint32_t *)(g + 8) = (ll >= 0) ? (uint32_t)ll : mh.msg_namelen;
                    if (ll < 0 && mh.msg_namelen)
                        memcpy(gname, &nss, mh.msg_namelen < gnamelen ? mh.msg_namelen : gnamelen);
                } else
                    *(uint32_t *)(g + 8) = mh.msg_namelen;
                int cmsg_trunc = 0;
                size_t ln = (gc && gcl) ? (size_t)cmsg_m2l(&mh, gc, gcl, 0, &cmsg_trunc) : 0;
                if (((int)a3 & 0x40000000) && gc && ln) cmsg_lx_set_cloexec_fds(gc, ln); // MSG_CMSG_CLOEXEC
                *(uint64_t *)(g + 40) = ln;
                int host_mflags = (int)mh.msg_flags;
                if (!cmsg_trunc && gc && gcl)
                    host_mflags &= ~MSG_CTRUNC; // host-side sideband expansion compressed cleanly
                int mfl = msgflags_m2l(host_mflags);
                if (cmsg_trunc) mfl |= 0x8; // MSG_CTRUNC
                *(uint32_t *)(g + 48) = (uint32_t)mfl;
            }
            if (hctl != hstack) free(hctl);
            done++;
        }
        G_RET(c) = (done == 0 && err) ? (uint64_t)(-(int64_t)err) : (uint64_t)done;
    mmsg_done:
        break;
    }
