    case 206: {
        void *send_payload __attribute__((cleanup(guest_free_bounce))) = NULL;
        void *send_destination __attribute__((cleanup(guest_free_bounce))) = NULL;
        if (a2) {
            send_payload = malloc((size_t)a2);
            if (!send_payload) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            if (guest_copy_from(send_payload, a1, (size_t)a2) != (ssize_t)a2) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            a1 = (uint64_t)(uintptr_t)send_payload;
        }
        if (a4) {
            if (a5 > 4096) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            send_destination = calloc(a5 ? (size_t)a5 : 1, 1);
            if (!send_destination) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            if (a5 && guest_copy_from(send_destination, a4, (size_t)a5) != (ssize_t)a5) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            a4 = (uint64_t)(uintptr_t)send_destination;
        }
        // A bad DEST-address pointer -> EFAULT, not an engine fault: the DNS/AF_UNIX/INET classifiers
        // below deref a4 directly. (The data buffer a1 is validated by the host sendto itself.) The dest
        // is optional (NULL on a connected socket), so only validate when present. (LTP sendto02 EFAULT)
        if (a4) {
            size_t dlc = (size_t)(socklen_t)a5;
            if (dlc > sizeof(struct sockaddr_storage)) dlc = sizeof(struct sockaddr_storage);
            if (!host_range_mapped((uintptr_t)a4, dlc)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
        }
        // Oversized UDP datagram -> EMSGSIZE, enforced before routing (matches Linux; the AF_UNIX switch
        // backing would otherwise leak a wrong errno for a too-large payload).
        {
            size_t udp_cap = udp_dgram_maxlen((int)a0);
            if (udp_cap && (size_t)a2 > udp_cap) {
                G_RET(c) = (uint64_t)(-EMSGSIZE);
                break;
            }
        }
        // Container DNS: a query sent to 127.0.0.11:53 (connected send, or first unconnected sendto) is
        // parsed + answered via the host resolver; nothing hits the wire. a4/a5 are the optional dest addr.
        {
            int64_t dret;
            if (dns_try_send((int)a0, (const uint8_t *)a1, (size_t)a2, (const uint8_t *)a4, (socklen_t)a5, &dret)) {
                G_RET(c) = (uint64_t)dret;
                break;
            }
            if (icmp_try_send((int)a0, (const uint8_t *)a1, (size_t)a2, (const uint8_t *)a4, (socklen_t)a5, &dret)) {
                G_RET(c) = (uint64_t)dret;
                break;
            }
        }
        // MSG_NOSIGNAL(0x4000) has no per-call equivalent on macOS; emulate it with the SO_NOSIGPIPE
        // socket option so the send returns EPIPE instead of raising a fatal SIGPIPE.
        if ((int)a3 & 0x4000) { (void)hl_native_set_no_sigpipe((int)a0); }
        // AF_UNIX pathname/abstract dest -> overlay/abstract-route it (syslog `logger` -> /dev/log).
        if (a4 && (socklen_t)a5 >= 2 && *(const uint16_t *)a4 == AF_UNIX) {
            char uhost[1200];
            if (unix_dgram_dest((const uint8_t *)a4, (socklen_t)a5, uhost, sizeof uhost)) {
                struct iovec iov = {(void *)a1, (size_t)a2};
                struct msghdr mh;
                memset(&mh, 0, sizeof mh);
                mh.msg_iov = &iov;
                mh.msg_iovlen = 1;
                int64_t ur;
                do {
                    ur = unix_dgram_sendmsg_at((int)a0, uhost, &mh, msgflags_l2m((int)a3));
                } while (ur < 0 && SVC_EINTR_RESTART(c));
                G_RET(c) = ur < 0 ? (uint64_t)(-errno) : (uint64_t)ur;
                break;
            }
        }
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_dgram[(int)a0]) {
            char udp_path[200];
            int udp_interface;
            uint32_t udp_ip;
            uint16_t udp_port;
            int udp_route = a4 ? udp_switch_destination((const uint8_t *)a4, (socklen_t)a5, &udp_interface, &udp_ip,
                                                        &udp_port, udp_path, sizeof udp_path)
                               : udp_switch_peer_path((int)a0, udp_path, sizeof udp_path);
            if (udp_route < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if (udp_route > 0) {
                if (!a4) udp_interface = (int)g_udp_peer_interface[(int)a0] - 1;
                if (udp_switch_ensure_source((int)a0, udp_interface) < 0) {
                    G_RET(c) = (uint64_t)(-errno);
                    break;
                }
                struct sockaddr_un un;
                ssize_t ur = unix_addr_set(&un, udp_path) < 0
                                 ? -1
                                 : sendto((int)a0, (void *)a1, (size_t)a2, msgflags_l2m((int)a3),
                                          (struct sockaddr *)&un, sizeof un);
                // Unconnected UDP send to a loopback port with no bound receiver: Linux drops the datagram
                // and reports success (no ICMP error is delivered to an unconnected socket). The AF_UNIX
                // switch backing instead surfaces ENOENT/ECONNREFUSED from the missing peer path -- coerce
                // it to a fire-and-forget success. Only for an explicit dest (a4); a connected socket (peer
                // path) keeps the underlying error so ICMP port-unreach can map to ECONNREFUSED.
                if (ur < 0 && a4 && (errno == ENOENT || errno == ECONNREFUSED)) {
                    G_RET(c) = (uint64_t)a2;
                    break;
                }
                G_RET(c) = ur < 0 ? (uint64_t)(-errno) : (uint64_t)ur;
                break;
            }
        }
        // dest addr (UDP): translate Linux AF_INET/INET6 sockaddr -> macOS; NULL/non-inet pass through.
        struct sockaddr_storage dss;
        socklen_t dhl = a4 ? sa_l2m((uint8_t *)a4, (socklen_t)a5, &dss) : (socklen_t)-1;
        const void *dst = (dhl != (socklen_t)-1) ? (void *)&dss : (void *)a4;
        socklen_t dl = (dhl != (socklen_t)-1) ? dhl : (socklen_t)a5;
        // #261 IPv4-only network: an external IPv6 datagram dest has no route -> ENETUNREACH now (mirrors the
        // connect() path; a QUIC/DoH client's v6 attempt fails fast and it retries over IPv4).
        if (dhl != (socklen_t)-1 && v6_no_route((struct sockaddr *)&dss)) {
            G_RET(c) = (uint64_t)(-ENETUNREACH);
            break;
        }
        ssize_t r;
        do {
            r = sendto((int)a0, (void *)a1, (size_t)a2, msgflags_l2m((int)a3), dst, dl);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        // send/sendto to a peer-closed socket -> guest SIGPIPE (Linux), unless MSG_NOSIGNAL was requested.
        if (!((int)a3 & 0x4000)) svc_sigpipe_on_epipe(c, (int64_t)G_RET(c));
        break;
    }
    case 207: {
        uint64_t guest_buffer = a1, guest_source = a4, guest_source_length = a5;
        socklen_t source_capacity = 0, source_length = 0;
        void *receive_buffer __attribute__((cleanup(guest_free_bounce))) = NULL;
        void *receive_source __attribute__((cleanup(guest_free_bounce))) = NULL;
        if (a2) {
            receive_buffer = malloc((size_t)a2);
            if (!receive_buffer) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            a1 = (uint64_t)(uintptr_t)receive_buffer;
        }
        if (a4) {
            if (!a5 || guest_copy_from(&source_capacity, a5, sizeof(source_capacity)) != sizeof(source_capacity)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            size_t source_bytes =
                source_capacity > sizeof(struct sockaddr_storage) ? source_capacity : sizeof(struct sockaddr_storage);
            if (source_bytes > 4096) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            receive_source = calloc(source_bytes, 1);
            if (!receive_source) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            source_length = source_capacity;
            a4 = (uint64_t)(uintptr_t)receive_source;
            a5 = (uint64_t)(uintptr_t)&source_length;
        }
        // src addr: receive into host scratch (macOS layout) then translate back to Linux for the guest.
        struct sockaddr_storage hss;
        socklen_t hsl = sizeof hss;
        int want = a4 != 0;
        // Zero-length address-peek idiom: force macOS to block + fill the sender via a 1-byte scratch.
        char one;
        int peekaddr = dgram_addr_peek((int)a0, want, (size_t)a2);
        void *rbuf = peekaddr ? &one : (void *)a1;
        size_t rlen = peekaddr ? 1 : (size_t)a2;
        ssize_t r;
        ts_wait_enter(); // 'S' while blocked in recvfrom/recv
        do {
            hsl = sizeof hss;
            r = recvfrom((int)a0, rbuf, rlen, msgflags_l2m((int)a3), want ? (struct sockaddr *)&hss : NULL,
                         want ? &hsl : NULL);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        ts_wait_leave();
        if (r > 0 && peekaddr) r = 0; // guest asked for 0 bytes; the address is what it wanted
        // SEQPACKET-as-DGRAM EOF: a peer-closed DGRAM recv reports ECONNRESET, but Linux SEQPACKET
        // returns 0 (EOF). Translate so the guest sees the expected end-of-stream. (See case 199.)
        if (r < 0 && errno == ECONNRESET && seq_is((int)a0)) r = 0;
        if (r >= 0 && want && (int)a0 >= 0 && (int)a0 < HL_NFD && g_icmp_sock[(int)a0]) {
            fill_inet_br((uint8_t *)a4, (socklen_t *)a5, g_icmp_ip[(int)a0], 0);
        } else if (r >= 0 && want && (int)a0 >= 0 && (int)a0 < HL_NFD && g_dns_sock[(int)a0]) {
            // DNS socket: report the source as the nameserver (127.0.0.11:53) so the guest resolver's
            // "answer came from the server we queried" anti-spoof check passes (the real src is AF_UNIX).
            dns_fill_ns((int)a0, (uint8_t *)a4, (socklen_t *)a5);
        } else if (r >= 0 && want &&
                   udp_switch_source((int)a0, &hss, hsl, (uint8_t *)a4, (socklen_t *)a5)) {
            // translated from the switch's opaque AF_UNIX sender identity
        } else if (r >= 0 && want) {
            socklen_t gcap = a5 ? *(socklen_t *)a5 : 0;
            int ll = sa_m2l((struct sockaddr *)&hss, (uint8_t *)a4, gcap);
            if (ll < 0) ll = sa_un_m2l((struct sockaddr *)&hss, hsl, (uint8_t *)a4, gcap);
            if (ll < 0) {
                socklen_t n = hsl < gcap ? hsl : gcap;
                if (gcap) memcpy((void *)a4, &hss, n);
                if (a5) *(socklen_t *)a5 = hsl;
            } else if (a5)
                *(socklen_t *)a5 = (socklen_t)ll;
        }
        if (r >= 0 && r > 0 && guest_copy_to(guest_buffer, receive_buffer, (size_t)r) != r) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        if (r >= 0 && guest_source) {
            size_t copy_length = source_capacity < source_length ? source_capacity : source_length;
            if ((copy_length && guest_copy_to(guest_source, receive_source, copy_length) != (ssize_t)copy_length) ||
                guest_copy_to(guest_source_length, &source_length, sizeof(source_length)) != sizeof(source_length)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // setsockopt(fd, level, optname, val, len)
    case 208: {
        void *option_value __attribute__((cleanup(guest_free_bounce))) = NULL;
        if (a3 && a4) {
            if (a4 > 1024 * 1024) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            option_value = malloc((size_t)a4);
            if (!option_value) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            if (guest_copy_from(option_value, a3, (size_t)a4) != (ssize_t)a4) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            a3 = (uint64_t)(uintptr_t)option_value;
        }
        int lvl = (int)a1, opt = (int)a2;
        // SO_REUSEPORT (Linux SOL_SOCKET/15): remember the guest's intent so getsockopt reports it even when
        // this INET socket is later backed by an AF_UNIX switch socket (which reads SO_REUSEPORT back as 0).
        // The real setsockopt still runs below (dual-bind on the switch relies on it); this only tracks readback.
        if (lvl == 1 && opt == 15 && (int)a0 >= 0 && (int)a0 < HL_NFD)
            g_so_reuseport[(int)a0] = (a3 && (socklen_t)a4 >= 4 && *(int *)a3) ? 1 : 0;
        // SO_PASSCRED (Linux SOL_SOCKET/16): macOS has no equivalent. Record it per-fd so recvmsg(212)
        // synthesizes the SCM_CREDENTIALS ancillary record the Linux kernel would auto-attach (credential-aware
        // credential-aware IPC bootstrap requires it). Never fail the guest.
        if (lvl == 1 && opt == 16) {
            // Validate the fd like the real kernel before recording state: a closed fd is EBADF, a non-socket
            // is ENOTSOCK. SO_TYPE succeeds for any socket and reproduces both errnos on the host.
            int st_;
            socklen_t stl_ = sizeof st_;
            if (getsockopt((int)a0, SOL_SOCKET, SO_TYPE, &st_, &stl_) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            int on = (a3 && (socklen_t)a4 >= 4) ? *(int *)a3 : 0;
#if defined(__linux__)
            if (setsockopt((int)a0, SOL_SOCKET, SO_PASSCRED, &on, sizeof on) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
#endif
            if ((int)a0 >= 0 && (int)a0 < HL_NFD) g_sock_passcred[(int)a0] = on ? 1 : 0;
            G_RET(c) = 0;
            break;
        }
        // SO_RCVTIMEO(20)/SO_SNDTIMEO(21) (+ the 64-bit-time _NEW variants 66/67 glibc may use): a real
        // recv/send timeout the guest expects to ARM (a blocking recv with no data must return EAGAIN after
        // it, not hang forever). so_opt_l2m maps these to -1 (ignore) -> they were silently dropped. Translate
        // the Linux sock_timeval {s64 tv_sec; s64 tv_usec} (16B on 64-bit) into the macOS struct timeval and
        // set the real macOS option, reporting the true errno.
        if (lvl == 1 && (opt == 20 || opt == 21 || opt == 66 || opt == 67)) {
            struct timeval tv;
            memset(&tv, 0, sizeof tv);
            if (a3 && (socklen_t)a4 >= 16) {
                tv.tv_sec = (time_t)*(int64_t *)a3;
                tv.tv_usec = (suseconds_t) * (int64_t *)((uint8_t *)a3 + 8);
            }
            int mo = (opt == 21 || opt == 67) ? SO_SNDTIMEO : SO_RCVTIMEO;
            int r = setsockopt((int)a0, SOL_SOCKET, mo, &tv, sizeof tv);
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        // IPPROTO_TCP integer options on a tracked guest INET stream socket: once bind/connect swaps its host
        // backing to an AF_UNIX switch socket, the host rejects IPPROTO_TCP setsockopt with ENOPROTOOPT, but
        // Linux round-trips these on a real TCP socket and apps set TCP_NODELAY *after* connect(). Record the
        // guest's value so a later getsockopt reports it, and best-effort apply it to the host fd (a genuine
        // unswapped AF_INET socket still honors it). Never fail the guest.
        if (lvl == 6 && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_stream[(int)a0]) {
            int slot = tcp_shadow_slot((int)a2);
            if (slot >= 0) {
                int val = (a3 && (socklen_t)a4 >= 4) ? *(int *)a3 : 0;
                g_tcp_optval[(int)a0][slot] = val;
                g_tcp_optset[(int)a0][slot] = 1;
                int mo = tcp_opt_l2m((int)a2);
                if (mo >= 0) (void)setsockopt((int)a0, IPPROTO_TCP, mo, &val, sizeof val);
                G_RET(c) = 0;
                break;
            }
        }
        // IPPROTO_IP integer options on a tracked AF_INET socket: same story as TCP -- the AF_UNIX switch
        // backing rejects them with ENOPROTOOPT while native round-trips them. Record + best-effort apply to
        // the host fd (an unswapped socket still honors it). Only options native accepts on a connected
        // socket are shadow-slotted; the rest fall through to the real setsockopt (true ENOPROTOOPT/EPERM).
        if (lvl == 0 && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_fam[(int)a0] == AF_INET) {
            int slot = ip_shadow_slot((int)a2);
            if (slot >= 0) {
                int val = (a3 && (socklen_t)a4 >= 4) ? *(int *)a3 : 0;
                g_ipopt_val[(int)a0][slot] = val;
                g_ipopt_set[(int)a0][slot] = 1;
                int mo = ip_opt_l2m((int)a2);
                if (mo >= 0) (void)setsockopt((int)a0, IPPROTO_IP, mo, &val, sizeof val);
                G_RET(c) = 0;
                break;
            }
        }
        // IPPROTO_IPV6 integer options on a tracked AF_INET6 socket. IPV6_V6ONLY(26) is special: native
        // rejects a change once the socket is bound/connected (EINVAL), so try the real setsockopt first --
        // it succeeds pre-bind (unswapped) and its success/failure decides the shadow update and the errno.
        if (lvl == 41 && (int)a0 >= 0 && (int)a0 < HL_NFD && g_sock_fam[(int)a0] == LX_AF_INET6_FAM) {
            if ((int)a2 == 26) { // IPV6_V6ONLY
                int val = (a3 && (socklen_t)a4 >= 4) ? *(int *)a3 : 0;
                int mo = ip6_opt_l2m(26);
                int r = (mo >= 0) ? setsockopt((int)a0, IPPROTO_IPV6, mo, &val, sizeof val) : -1;
                if (r == 0) { // pre-bind, host honored it -> record and report the new value
                    g_ipopt_val[(int)a0][IPOPT_V6ONLY_SLOT] = val ? 1 : 0;
                    g_ipopt_set[(int)a0][IPOPT_V6ONLY_SLOT] = 1;
                    G_RET(c) = 0;
                } else {
                    // Swapped backing rejected it: native cannot change V6ONLY after bind/connect -> EINVAL.
                    G_RET(c) = (uint64_t)(-EINVAL);
                }
                break;
            }
            int slot = ip6_shadow_slot((int)a2);
            if (slot >= 0) {
                int val = (a3 && (socklen_t)a4 >= 4) ? *(int *)a3 : 0;
                g_ipopt_val[(int)a0][slot] = val;
                g_ipopt_set[(int)a0][slot] = 1;
                int mo = ip6_opt_l2m((int)a2);
                if (mo >= 0) (void)setsockopt((int)a0, IPPROTO_IPV6, mo, &val, sizeof val);
                G_RET(c) = 0;
                break;
            }
        }
        if (lvl == 1) {
            lvl = SOL_SOCKET;
            opt = so_opt_l2m((int)a2);
            if (opt < 0) {
                G_RET(c) = 0;
                break;
            }
            // translate SOL_SOCKET; ignore unknown
        } else if (lvl == 6) { // IPPROTO_TCP: optnames diverge — translate, ignore unknown (never cork by accident)
            opt = tcp_opt_l2m((int)a2);
            if (opt < 0) {
                G_RET(c) = 0;
                break;
            }
        } else if (lvl == 41) { // IPPROTO_IPV6: optnames diverge — translate (esp. IPV6_V6ONLY), ignore unknown
            opt = ip6_opt_l2m((int)a2);
            if (opt < 0) {
                G_RET(c) = 0;
                break;
            }
        } else if (lvl == 0) { // IPPROTO_IP: optnames diverge — translate (IP_TOS/TTL/HDRINCL/mcast), ignore unknown
            opt = ip_opt_l2m((int)a2);
            if (opt < 0) {
                G_RET(c) = 0;
                break;
            }
        }
        int r = setsockopt((int)a0, lvl, opt, (void *)a3, (socklen_t)a4);
        // A KNOWN-unsupported-but-harmless option already short-circuited to success above (opt<0). Anything
        // that reaches here is a real op on a translated/passthrough option; surface its true errno instead
        // of masking EINVAL/ENOPROTOOPT/EPERM (feature-probing code needs the real result).
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // getsockopt(fd, level, optname, val, len)
    case 209: {
        net_option_copyout option_copyout __attribute__((cleanup(net_option_copyout_finish)));
        int copyout_error = net_option_copyout_begin(&option_copyout, c, a3, a4);
        if (copyout_error < 0) {
            G_RET(c) = (uint64_t)(int64_t)copyout_error;
            break;
        }
        if (option_copyout.active) {
            a3 = (uint64_t)(uintptr_t)option_copyout.value;
            a4 = (uint64_t)(uintptr_t)&option_copyout.length;
        }
        int lvl = (int)a1, opt = (int)a2;
        int gfd = (int)a0;
        // A deferred asynchronous connect error (stashed for a non-blocking dial to a closed private-loopback
        // port) is delivered exactly once through SO_ERROR, mirroring a real TCP stack.
        if (lvl == 1 && opt == 4 && gfd >= 0 && gfd < HL_NFD && g_so_error[gfd]) {
            int deferred_error = sock_internal_async_error_take(gfd);
            if (a3 && a4 && *(socklen_t *)a4 >= 4) {
                *(int *)a3 = hl_linux_errno_from_host(deferred_error);
                *(socklen_t *)a4 = 4;
            }
            sock_internal_connect_observed(gfd, deferred_error);
            G_RET(c) = 0;
            break;
        }
        // SO_REUSEPORT(15): report the guest's recorded intent (an AF_UNIX switch socket always reads it
        // back as 0), for any tracked socket that had it set. An un-set fd falls through to the host value.
        if (lvl == 1 && opt == 15 && gfd >= 0 && gfd < HL_NFD && g_so_reuseport[gfd]) {
            if (a3 && a4 && *(socklen_t *)a4 >= 4) {
                *(int *)a3 = 1;
                *(socklen_t *)a4 = 4;
            }
            G_RET(c) = 0;
            break;
        }
        // SO_DOMAIN(39)/SO_PROTOCOL(38): a private-loopback/bridge INET socket is backed on the host by an
        // AF_UNIX switch socket, so a raw getsockopt would report AF_UNIX/0 rather than the guest's real
        // AF_INET[6]/IPPROTO_TCP[UDP]. Report the family/protocol recorded at socket() for any tracked INET
        // socket -- identical to the host answer for an un-swapped fd, corrected for a swapped one.
        if (lvl == 1 && (opt == 38 || opt == 39) && gfd >= 0 && gfd < HL_NFD &&
            (g_sock_fam[gfd] == AF_INET || g_sock_fam[gfd] == LX_AF_INET6_FAM)) {
            int val = 0, have = 1;
            if (opt == 39)
                val = g_sock_fam[gfd]; // SO_DOMAIN: the guest address family
            else if (g_sock_stream[gfd])
                val = IPPROTO_TCP;
            else if (g_sock_dgram[gfd])
                val = IPPROTO_UDP;
            else
                have = 0; // unknown protocol -> fall through to the host value
            if (have) {
                if (a3 && a4 && *(socklen_t *)a4 >= 4) {
                    *(int *)a3 = val;
                    *(socklen_t *)a4 = 4;
                }
                G_RET(c) = 0;
                break;
            }
        }
        // SO_PEERCRED (Linux SOL_SOCKET/17): macOS has no SO_PEERCRED. Report the peer's credentials as the
        // container identity (so cr.uid/gid match the guest's getuid/getgid) and the peer pid via macOS
        // LOCAL_PEERPID. struct ucred is { pid_t pid; uid_t uid; gid_t gid; } (3x u32 = 12 bytes).
        // SO_PASSCRED (16): report the per-fd flag we recorded at setsockopt (macOS has no such option).
        // Both SO_PASSCRED and SO_PEERCRED must first validate the fd like the kernel: EBADF for a closed fd,
        // ENOTSOCK for a regular file. Returning synthetic creds on a non-socket is fake success.
        if (lvl == 1 && (opt == 16 || opt == 17)) {
            int st_;
            socklen_t stl_ = sizeof st_;
            if (getsockopt((int)a0, SOL_SOCKET, SO_TYPE, &st_, &stl_) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
        }
        if (lvl == 1 && opt == 16) {
            if (a3 && a4 && *(socklen_t *)a4 >= 4) {
                *(int *)a3 = ((int)a0 >= 0 && (int)a0 < HL_NFD) ? g_sock_passcred[(int)a0] : 0;
                *(socklen_t *)a4 = 4;
            }
            G_RET(c) = 0;
            break;
        }
        if (lvl == 1 && opt == 17) {
            if (a3 && a4 && *(socklen_t *)a4 >= 12) {
                pid_t ppid = 0;
#if defined(__APPLE__)
                socklen_t pl = sizeof ppid;
                if (getsockopt((int)a0, SOL_LOCAL, LOCAL_PEERPID, &ppid, &pl) < 0 || ppid <= 0 || ppid == getpid()) {
#else
                struct ucred peer = {0};
                socklen_t pl = sizeof peer;
                if (getsockopt((int)a0, SOL_SOCKET, SO_PEERCRED, &peer, &pl) == 0) ppid = peer.pid;
                if (ppid <= 0 || ppid == getpid()) {
#endif
                    // macOS reports the socketpair CREATOR's pid on both ends -> a fork parent
                    // reads its OWN pid here for every child. Report the end's peer pid we resolved (the REAL
                    // guest pid of the process holding the OTHER end, stamped across fork/close -- see
                    // g_sock_peer_pid / seq_reassign_peer); else this guest's own pid.
                    int sp = ((int)a0 >= 0 && (int)a0 < HL_NFD) ? g_sock_peer_pid[(int)a0] : 0;
#if defined(__linux__)
                    // On Linux the host SO_PEERCRED is authoritative: ppid==getpid() means the peer end is
                    // held in THIS process (an un-forked socketpair), whose guest pid is our own. A synthetic
                    // distinct id here would make SO_PEERCRED disagree with the guest's getpid() for a plain
                    // socketpair; report the guest self pid, keeping the synthetic only as a last resort.
                    ppid = (ppid == getpid()) ? container_pid() : (sp ? sp : container_pid());
#else
                    ppid = sp ? sp : container_pid();
#endif
                } else {
                    int guest_peer;
                    if (hl_linux_pidmap_guest_checked(&g_pidmap, (int)ppid, &guest_peer) != 0) {
                        G_RET(c) = (uint64_t)(int64_t)-ESRCH;
                        break;
                    }
                    ppid = (!hl_linux_pidmap_is_active(&g_pidmap) && g_init_hostpid && ppid == g_init_hostpid) ? 1 : guest_peer;
                }
                uint32_t *u = (uint32_t *)a3;
                u[0] = (uint32_t)ppid; // pid (resolved above)
                // NOTE: peer uid/gid are reported as this container's identity, NOT the peer's real guest
                // uid/gid. A truthful per-peer value is infeasible here: (a) macOS LOCAL_PEERCRED yields the
                // peer's HOST uid, but every container process runs under the same host uid (guest uids are
                // emulated), and guest setuid is ownership-only (see setfsuid note), so LOCAL_PEERCRED can't
                // reflect a guest that dropped privileges; (b) cross-process we have no channel to the peer's
                // emulated guest uid. cuid()/cgid() is the closest available. Impact: Postgres `peer`/ident
                // auth and polkit/systemd uid checks see the container identity, not a setuid'd client uid.
                u[1] = (uint32_t)cuid(); // uid (see NOTE: container identity, not the peer's true guest uid)
                u[2] = (uint32_t)cgid(); // gid (see NOTE)
                *(socklen_t *)a4 = 12;
            }
            G_RET(c) = 0;
            break;
        }
        // SO_RCVTIMEO(20)/SO_SNDTIMEO(21) (+ _NEW 66/67): report the armed timeout back in the Linux
        // sock_timeval {s64 tv_sec; s64 tv_usec} layout, translated from the macOS struct timeval.
        if (lvl == 1 && (opt == 20 || opt == 21 || opt == 66 || opt == 67)) {
            struct timeval tv;
            memset(&tv, 0, sizeof tv);
            socklen_t tl = sizeof tv;
            int mo = (opt == 21 || opt == 67) ? SO_SNDTIMEO : SO_RCVTIMEO;
            int r = getsockopt((int)a0, SOL_SOCKET, mo, &tv, &tl);
            if (r < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            if (a3 && a4 && *(socklen_t *)a4 >= 16) {
                *(int64_t *)a3 = (int64_t)tv.tv_sec;
                *(int64_t *)((uint8_t *)a3 + 8) = (int64_t)tv.tv_usec;
                *(socklen_t *)a4 = 16;
            }
            G_RET(c) = 0;
            break;
        }
        // IPPROTO_TCP integer options on a tracked guest INET stream socket: report the shadowed value the
        // guest set (see case 208). If unset, prefer the real host value for a still-genuine AF_INET socket,
        // falling back to a stable default only when the AF_UNIX switch backing rejects the query.
        if (lvl == 6 && gfd >= 0 && gfd < HL_NFD && g_sock_stream[gfd]) {
            int slot = tcp_shadow_slot((int)a2);
            if (slot >= 0) {
                int val;
                if (g_tcp_optset[gfd][slot]) {
                    val = g_tcp_optval[gfd][slot];
                } else {
                    val = tcp_shadow_default(slot);
                    int hv;
                    socklen_t hl = sizeof hv;
                    int mo = tcp_opt_l2m((int)a2);
                    if (mo >= 0 && getsockopt(gfd, IPPROTO_TCP, mo, &hv, &hl) == 0) val = hv;
                }
                if (a3 && a4 && *(socklen_t *)a4 >= 4) {
                    *(int *)a3 = val;
                    *(socklen_t *)a4 = 4;
                }
                G_RET(c) = 0;
                break;
            }
            // TCP_INFO(11): a struct the AF_UNIX switch backing cannot answer. Try the host first (an unswapped
            // AF_INET socket returns the real thing); on rejection synthesize a minimal record whose only stable
            // fact is tcpi_state (byte 0) = ESTABLISHED for a connected socket, so diagnostic code sees a live
            // connection instead of ENOPROTOOPT. The rest is zero-filled.
            if ((int)a2 == 11 && a3 && a4) {
                socklen_t cap = *(socklen_t *)a4;
#if defined(__linux__)
                // Linux TCP_INFO == 11; an unswapped AF_INET socket answers it authoritatively.
                if (getsockopt(gfd, IPPROTO_TCP, 11, (void *)a3, (socklen_t *)a4) == 0) {
                    G_RET(c) = 0;
                    break;
                }
#endif
                socklen_t n = cap < 512 ? cap : 512;
                memset((void *)a3, 0, n);
                if (n >= 1) *(uint8_t *)a3 = g_sock_conn[gfd] ? 1 /*TCP_ESTABLISHED*/ : 7 /*TCP_CLOSE*/;
                *(socklen_t *)a4 = n;
                G_RET(c) = 0;
                break;
            }
        }
        // IPPROTO_IP integer options on a tracked AF_INET socket: report the shadowed value the guest set.
        // If unset, prefer the real host value (a still-genuine AF_INET socket answers it), falling back to
        // the Linux default only when the AF_UNIX switch backing rejects the query.
        if (lvl == 0 && gfd >= 0 && gfd < HL_NFD && g_sock_fam[gfd] == AF_INET) {
            int slot = ip_shadow_slot((int)a2);
            if (slot >= 0) {
                int val;
                if (g_ipopt_set[gfd][slot]) {
                    val = g_ipopt_val[gfd][slot];
                } else {
                    val = ipopt_shadow_default(slot);
                    int hv;
                    socklen_t hl = sizeof hv;
                    int mo = ip_opt_l2m((int)a2);
                    if (mo >= 0 && getsockopt(gfd, IPPROTO_IP, mo, &hv, &hl) == 0) val = hv;
                }
                if (a3 && a4 && *(socklen_t *)a4 >= 4) {
                    *(int *)a3 = val;
                    *(socklen_t *)a4 = 4;
                }
                G_RET(c) = 0;
                break;
            }
        }
        // IPPROTO_IPV6 integer options on a tracked AF_INET6 socket, including IPV6_V6ONLY(26) whose readback
        // must survive bind (dual-stack servers set it v6-only then read it back).
        if (lvl == 41 && gfd >= 0 && gfd < HL_NFD && g_sock_fam[gfd] == LX_AF_INET6_FAM) {
            int slot = ((int)a2 == 26) ? IPOPT_V6ONLY_SLOT : ip6_shadow_slot((int)a2);
            if (slot >= 0) {
                int val;
                if (g_ipopt_set[gfd][slot]) {
                    val = g_ipopt_val[gfd][slot];
                } else {
                    val = ipopt_shadow_default(slot);
                    int hv;
                    socklen_t hl = sizeof hv;
                    int mo = ip6_opt_l2m((int)a2);
                    if (mo >= 0 && getsockopt(gfd, IPPROTO_IPV6, mo, &hv, &hl) == 0) val = hv;
                }
                if (a3 && a4 && *(socklen_t *)a4 >= 4) {
                    *(int *)a3 = val;
                    *(socklen_t *)a4 = 4;
                }
                G_RET(c) = 0;
                break;
            }
        }
        if (lvl == 1) {
            lvl = SOL_SOCKET;
            opt = so_opt_l2m((int)a2);
            if (opt < 0) { // genuinely-unknown SOL_SOCKET optname -> Linux getsockopt returns ENOPROTOOPT
                G_RET(c) = (uint64_t)(-ENOPROTOOPT);
                break;
            }
        } else if (lvl == 6) { // IPPROTO_TCP: translate optname; unknown -> ENOPROTOOPT
            opt = tcp_opt_l2m((int)a2);
            if (opt < 0) {
                G_RET(c) = (uint64_t)(-ENOPROTOOPT);
                break;
            }
        } else if (lvl == 41) { // IPPROTO_IPV6: translate optname (esp. IPV6_V6ONLY); unknown -> ENOPROTOOPT
            opt = ip6_opt_l2m((int)a2);
            if (opt < 0) {
                G_RET(c) = (uint64_t)(-ENOPROTOOPT);
                break;
            }
        } else if (lvl == 0) { // IPPROTO_IP: translate optname; unknown -> ENOPROTOOPT
            opt = ip_opt_l2m((int)a2);
            if (opt < 0) {
                G_RET(c) = (uint64_t)(-ENOPROTOOPT);
                break;
            }
        }
        int socket_error_query = lvl == SOL_SOCKET && opt == SO_ERROR;
        int r = getsockopt((int)a0, lvl, opt, (void *)a3, (socklen_t *)a4);
        if (r == 0 && socket_error_query && gfd >= 0 && gfd < HL_NFD && g_sock_connecting[gfd] && a3 && a4 &&
            *(socklen_t *)a4 >= (socklen_t)sizeof(int))
            sock_internal_connect_observed(gfd, *(int *)a3);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 210:
        G_RET(c) = sock_internal_shutdown((int)a0, (int)a1) < 0 ? (uint64_t)(-errno) : 0;
        // shutdown(fd, how) -- SHUT_RD/WR/RDWR match
        break;
    case 211:
    // sendmsg/recvmsg -- translate Linux msghdr -> macOS
