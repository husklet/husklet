/* Coordinator-only publication and process-tree rendezvous. Included by image.c so it shares the
 * checkpoint translation unit while keeping the capture state machine behind a named boundary. */

static void ckpt_publish_manifest(const struct ckpt_phase_ledger *phases, struct ckpt_sink *sink, int nfoll,
                                  int nexempt) {
    // Publish the MANIFEST last: its presence == a complete, restorable checkpoint.
    //
    // SEAL FIRST, THEN COUNT, AND COUNT AGAINST THE SEAL. The expected process set is fixed at exactly one
    // instant, here, and it comes from the broker's REGISTER_READY ledger rather than from anything this
    // process enumerated. That matters in both directions, and both have been observed in production:
    //
    //   - a process forked after the coordinator's scan is a genuine member that commits a genuine group.
    //     Measured against an enumeration it postdates it reads as a surplus group and refuses a healthy
    //     close; measured against the ledger it registered in, it is simply one of the members.
    //   - a process the coordinator never enumerated, or enumerated and lost, that DID register is a
    //     member whose state is unsaved. An enumeration-derived count cannot see it at all -- that is the
    //     shape that published `checkpoint OK: 1 process(es)` with a registered member missing. A sealed
    //     count is higher than the committed groups and refuses.
    //
    // The seal runs after this coordinator's own dump, so every member including the init is in the ledger,
    // and after the rendezvous, so no unfrozen guest process remains that could still fork or register. A
    // registration arriving after it is refused by the broker rather than admitted, and the late member
    // refuses its own dump instead of publishing into an image that is already being counted.
    uint64_t phase = ckpt_phase_begin(phases);
    uint64_t sealed = 0;
    if (ckpt_stream_seal_membership(&sealed) != 0)
        ckpt_coordinator_refuse(phases, CKPT_REFUSAL_PROCESS_COUNT,
                                "the broker could not seal this capture's membership, so the set of processes the "
                                "manifest must contain is unknown");
    int nproc = ckpt_sink_group_count(sink, "proc.");
    ckpt_phase_finish(phases, "settlement", phase, 0);
    if (nproc < 0 || sealed > (uint64_t)INT_MAX || nproc != (int)sealed) {
        char reason[HL_CKPT_STREAM_NAME_MAX];
        snprintf(reason, sizeof reason,
                 "process-count mismatch: %llu process(es) proved membership of this capture and exactly that "
                 "many groups must be committed, but %d were; %d peer(s) were enumerated and %d exempted",
                 (unsigned long long)sealed, nproc, nfoll, nexempt);
        ckpt_coordinator_refuse(phases, CKPT_REFUSAL_PROCESS_COUNT, reason);
    }
    struct ckpt_manifest man;
    phase = ckpt_phase_begin(phases);
    memset(&man, 0, sizeof man);
    man.magic = CKPT_MANIFEST_MAGIC;
    man.version = CKPT_VERSION;
    man.arch = G_CKPT_ARCH;
    man.n_procs = (uint64_t)nproc;
    man.root_gpid = 1;
    // Record which group owns the controlling terminal's foreground (in guest terms). The init is the tty's
    // session leader here, so tcgetpgrp reads the real fg host pgid; child job groups pass through untranslated
    // (guest pgid == host pgid), only the init's own group folds to guest pgid 1.
    {
        int tf = ckpt_ctty_open();
        int fgh = (tf >= 0) ? tcgetpgrp(tf) : -1;
        struct termios tio;
        if (fgh <= 0)
            man.fg_pgid_gpid = 0;
        else if (hl_linux_pidmap_guest_checked(&g_pgidmap, (int32_t)fgh, &man.fg_pgid_gpid) != 0) {
            ckpt_ctty_close(tf);
            ckpt_coordinator_refuse(phases, CKPT_REFUSAL_FOREGROUND_GROUP,
                                    "the terminal's foreground process group is outside the restored namespace");
        } else if (!hl_linux_pidmap_is_active(&g_pgidmap) && g_init_hostpid && fgh == g_init_hostpid)
            man.fg_pgid_gpid = 1;
        if (tf >= 0 && tcgetattr(tf, &tio) == 0) {
            size_t cc = sizeof tio.c_cc < sizeof man.tty_cc ? sizeof tio.c_cc : sizeof man.tty_cc;
            man.tty_termios = 1;
            man.tty_iflag = (uint32_t)tio.c_iflag;
            man.tty_oflag = (uint32_t)tio.c_oflag;
            man.tty_cflag = (uint32_t)tio.c_cflag;
            man.tty_lflag = (uint32_t)tio.c_lflag;
            man.tty_ispeed = (uint32_t)cfgetispeed(&tio);
            man.tty_ospeed = (uint32_t)cfgetospeed(&tio);
            memcpy(man.tty_cc, tio.c_cc, cc);
        }
        ckpt_ctty_close(tf);
    }
    // The digest is asked of the sink: the server accumulated it while the bytes went past, so nothing
    // re-reads the embedder's store.
    // Both of these fail from a checkpoint-stream PROTOCOL STATUS and set no errno anywhere on the path,
    // so `strerror(errno)` here reported whatever unrelated syscall had last set it -- in practice the
    // ENOTTY from this function's own tcgetattr on a non-tty, twenty lines above. Every socket-topology
    // refusal in this engine consequently blamed a terminal that was never involved. The sink now reports
    // its own outcome and the refusal names that.
    struct ckpt_sink_outcome outcome;
    char detail[256];
    if (ckpt_sink_digest(sink, &man.image_hash, &man.image_files, &man.image_bytes, &outcome) != 0) {
        char reason[HL_CKPT_STREAM_NAME_MAX];
        ckpt_sink_outcome_describe(&outcome, detail, sizeof detail);
        snprintf(reason, sizeof reason, "cannot hash the checkpoint image: %s", detail);
        ckpt_coordinator_refuse(phases, CKPT_REFUSAL_DIGEST, reason);
    }
    // Explicit completion: the only signal that the image is complete.
    if (ckpt_sink_commit(sink, &man, sizeof man, &outcome) != 0) {
        char reason[HL_CKPT_STREAM_NAME_MAX];
        ckpt_sink_outcome_describe(&outcome, detail, sizeof detail);
        snprintf(reason, sizeof reason, "cannot publish the checkpoint manifest: %s", detail);
        ckpt_coordinator_refuse(phases, CKPT_REFUSAL_MANIFEST, reason);
    }
    ckpt_phase_finish(phases, "manifest_publication", phase, 0);
    fprintf(stderr, "[ckpt] checkpoint OK: %d process(es)\n", nproc);
    int st;
    phase = ckpt_phase_begin(phases);
    // Final reap, deliberately NOT recording: the manifest is already published, so a status destroyed here
    // could not be carried anyway. Nothing unrecorded can reach it -- the rendezvous ended quiescent, every
    // member is parked inside its own freeze and cannot run guest code, and therefore cannot fork or exit,
    // until the host releases it after this point. This collects the corpses of members that already exited
    // during the capture, whose groups are in the image.
    while (waitpid(-1, &st, WNOHANG) > 0) {} // final reap
    ckpt_phase_finish(phases, "native_reap", phase, 0);
    hl_engine_child_result_publish(0, HL_STATUS_OK, 0);
    ckpt_phase_exit(phases, 0);
}

static void ckpt_coordinate_and_exit(struct cpu *c) {
    const struct ckpt_phase_ledger phases = {
        .enabled = hl_option_get("HL_CHECKPOINT_PHASE_LEDGER") != NULL,
        .isa = ckpt_phase_isa_name(G_CKPT_ARCH),
        .generation = ckpt_request_generation(),
        .clock_failure = hl_option_get("HL_CHECKPOINT_PHASE_CLOCK_FAIL") != NULL,
        .descriptor = ckpt_phase_descriptor(),
    };
    uint64_t phase = ckpt_phase_begin(&phases);
    struct ckpt_sink *sink = ckpt_sink_current();

    /* THE MEMBER SET IS NOT A SNAPSHOT. `ckpt_live_process_peers` reads the tree at one instant, and an
     * ordinary guest tree forks and exits across that instant -- a shell's `while :; do ...; sleep .05;
     * done`, a `make` job, any transient child. A process forked immediately AFTER the scan is a real
     * guest process with real state: it reaches its own safepoint, observes the trigger generation, proves
     * membership and commits its group. Counting it against a set fixed before it existed is what produced
     * `process-count mismatch: expected exactly N committed groups, captured N+1` on a perfectly healthy
     * close. The mirror of the same hole is worse: a process the scan MISSED that never commits is a
     * member whose state is unsaved, and a count derived from the scan reports `checkpoint OK` anyway.
     *
     * So enumeration is demoted to what it can actually do -- find processes that need KICKING to a
     * safepoint -- and it is repeated. Every rendezvous pass rescans and adopts whatever appeared since
     * the last one, because a process that has already frozen cannot fork: each pass that finds nothing
     * new is a pass in which the unfrozen set was empty. The set the MANIFEST is checked against comes
     * from the broker instead (see the seal below), which is the only party that observes membership
     * rather than inferring it. */
    size_t scan_capacity = 512;
    hl_host_process_peer *scan = malloc(scan_capacity * sizeof *scan);
    size_t observed = 0;
    hl_host_process_peer *foll = NULL;
    unsigned char *completed = NULL;
    int nfoll = 0;
    int known_capacity = 0;
    if (scan == NULL)
        ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_RESOURCES, "cannot allocate the peer enumeration buffer");
    /* Test-only, and they model the two ways enumeration can be wrong about a real process rather than
     * approximating them. HIDDEN_FROM_ENUMERATION withholds one live member from the FIRST scan only,
     * which is indistinguishable downstream from a process that came into existence one instruction after
     * that scan returned. FORGOTTEN_AFTER_KICK lets one member be kicked -- so it really does prove
     * membership to the broker -- and then drops it from the known set and from every later scan, which is
     * the reported blind spot exactly: a peer that registered, exited, and was then enumerated as 0 peers.
     * Only the broker knows that process was ever a member. */
    int hide_first_scan = hl_option_get("HL_CKPT_TEST_PEER_HIDDEN_FROM_ENUMERATION") != NULL;
    int forget_after_kick = hl_option_get("HL_CKPT_TEST_PEER_FORGOTTEN_AFTER_KICK") != NULL;
    long long hidden = 0;
    int ndone = 0;
    int nexempt = 0;
    int quiet = 0;
    int stalled = 0;
    int churning = 0;
    /* Per known peer, the host CPU time it had consumed when it was last seen to advance. Parallel to
     * `foll`/`completed` and grown with them. */
    uint64_t *consumed = NULL;
    for (unsigned long long t = 0;; t++) {
        int settled = 0;
        // Reap BEFORE the liveness test below, not after: an unreaped child of ours is a zombie, and
        // kill(pid, 0) succeeds on a zombie, so an exited transient child would read as still reachable
        // for as long as it stayed unreaped and would never qualify for the exemption. Every status it
        // takes is RECORDED first -- that reap used to destroy the pending child status a guest parent was
        // blocked in wait4 for, and the restored parent then waited forever for a pid that never existed
        // again. See ckpt_record_reaped_child.
        ckpt_reap_and_record(&phases);
        // Rescan and adopt. A peer discovered here is kicked exactly as one found by the first scan is;
        // nothing else distinguishes them, because nothing else should.
        int discovered = 0;
        for (;;) {
            if (!ckpt_live_process_peers(scan, scan_capacity, &observed))
                ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_PEER_ENUMERATION, "cannot enumerate the live peer set");
            if (observed <= scan_capacity) break;
            if (observed > (size_t)INT_MAX || observed > SIZE_MAX / sizeof *scan)
                ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_PEER_ENUMERATION,
                                        "the live peer set is larger than the coordinator can address");
            hl_host_process_peer *expanded = realloc(scan, observed * sizeof *scan);
            if (expanded == NULL)
                ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_RESOURCES, "cannot grow the peer enumeration buffer");
            scan = expanded;
            scan_capacity = observed;
        }
        for (size_t index = 0; index < observed; index++) {
            if (!ckpt_capture_member(scan[index].identity, getpid())) continue;
            int already = 0;
            for (int i = 0; i < nfoll; i++)
                if (foll[i].identity == scan[index].identity) already = 1;
            if (already) continue;
            if (scan[index].identity == hidden) continue; /* forgotten: this scan never reports it again */
            if (hide_first_scan && t == 0) {
                hide_first_scan = 0;
                fprintf(stderr, "[ckpt] participant %lld withheld from the first enumeration (test hook)\n",
                        (long long)scan[index].identity);
                continue;
            }
            if (nfoll == known_capacity) {
                int grown = known_capacity == 0 ? 16 : known_capacity * 2;
                hl_host_process_peer *peers = realloc(foll, (size_t)grown * sizeof *foll);
                unsigned char *ledger = peers != NULL ? realloc(completed, (size_t)grown) : NULL;
                uint64_t *progress = ledger != NULL ? realloc(consumed, (size_t)grown * sizeof *consumed) : NULL;
                if (peers != NULL) foll = peers;
                if (ledger != NULL) completed = ledger;
                if (progress == NULL)
                    ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_RESOURCES, "cannot grow the rendezvous ledger");
                consumed = progress;
                known_capacity = grown;
            }
            foll[nfoll] = scan[index];
            completed[nfoll] = 0;
            consumed[nfoll] = 0;
            nfoll++;
            discovered = 1;
            // Freeze + dump this peer: the shared trigger generation is already advanced (the requester
            // bumped it), so KICK it with the guest-proof THREAD_INT_SIG to bounce it out of a blocked
            // syscall / chained in-cache loop to its safepoint, where ckpt_poll sees the new generation and
            // dumps proc.<gpid> + _exit()s.
            int kicked = hl_host_process_interrupt(scan[index]);
            fprintf(stderr, "[ckpt] participant %lld %s\n", (long long)scan[index].identity,
                    kicked ? "interrupted" : "NOT interrupted (it cannot reach a safepoint)");
            if (forget_after_kick) { /* kicked, so it can prove membership -- and then never enumerated again */
                int registered = 0;
                for (int pass = 0; pass < CKPT_RENDEZVOUS_STALL_PASSES; ++pass) {
                    registered = ckpt_stream_participant_registered(scan[index].identity);
                    if (registered == 1) break;
                    usleep(10000);
                }
                if (registered != 1) {
                    fprintf(stderr,
                            "[ckpt] test hook could not prove participant %lld registered before hiding it\n",
                            (long long)scan[index].identity);
                    ckpt_phase_exit(&phases, 71);
                }
                forget_after_kick = 0;
                hidden = scan[index].identity;
                nfoll--;
                discovered = 0;
                fprintf(stderr, "[ckpt] participant %lld dropped from the enumeration after its kick (test hook)\n",
                        (long long)hidden);
            }
        }
        for (int i = 0; i < nfoll; i++) {
            if (completed[i]) continue;
            char pd[64];
            snprintf(pd, sizeof pd, "proc.%d", ckpt_peer_gpid(foll[i].identity));
            // Rendezvous through the sink, not through the store: "that peer finished" is defined as
            // "its group was committed", which is exactly what group_commit means for every implementation.
            if (ckpt_sink_group_present(sink, pd) == 1) {
                completed[i] = 1;
                ndone++;
                settled = 1;
                continue;
            }
            // Committed is checked first, so a peer that committed and then exited is counted as the
            // member it is rather than exempted as a transient.
            if (ckpt_peer_never_contributed(foll[i].identity)) {
                fprintf(stderr,
                        "[ckpt] participant %lld exited before joining the capture (no REGISTER_READY for "
                        "generation %u, so it published nothing); not waiting for proc.%d\n",
                        (long long)foll[i].identity, phases.generation, ckpt_peer_gpid(foll[i].identity));
                completed[i] = 1;
                ndone++;
                nexempt++;
                settled = 1;
                continue;
            }
            /* Still outstanding: has it moved? A member that is merely starved keeps burning CPU, however
             * slowly; one that is wedged, or that never took the kick, burns none. Read it fresh every
             * pass -- a member that cannot be read at all (it has just died) is not progress, and it is
             * either exempted above on the next pass or refused by name below. */
            {
                hl_host_process_info info;
                uint64_t spent;
                if (hl_host_process_read(foll[i].identity, &info)) {
                    spent = info.user_time_ns + info.system_time_ns;
                    if (spent > consumed[i]) {
                        consumed[i] = spent;
                        settled = 1;
                    }
                }
            }
        }
        /* Quiescent means everything known has finished AND a rescan found nothing new -- and it takes
         * CKPT_ENUMERATION_QUIET_PASSES consecutive such passes, never one. One is not enough because the
         * very first pass is trivially quiet before anything has been adopted, and because a process
         * forked microseconds before the scan that missed it is discovered by the NEXT scan, not that one.
         * Settling on a single quiet pass is exactly the hole this loop exists to close: it would seal the
         * membership while an unfrozen guest process was still on its way to a safepoint. */
        quiet = ndone == nfoll && !discovered ? quiet + 1 : 0;
        if (quiet >= CKPT_ENUMERATION_QUIET_PASSES) break;
        /* `discovered` and `settled` cover the coarse milestones; `settled` also carries the CPU-time
         * advance recorded above. A pass in which none of them moved is a pass in which the whole
         * outstanding set stood still. */
        stalled = discovered || settled ? 0 : stalled + 1;
        if (stalled >= CKPT_RENDEZVOUS_STALL_PASSES) break;
        churning = discovered ? churning + 1 : 0;
        if (churning >= CKPT_RENDEZVOUS_CHURN_PASSES) break;
        usleep(10000);
    }
    free(scan);
    free(consumed);
    fprintf(stderr, "[ckpt] coordinator pid=%d found %d peer(s), %d exempt\n", getpid(), nfoll, nexempt);
    if (churning >= CKPT_RENDEZVOUS_CHURN_PASSES) {
        char reason[HL_CKPT_STREAM_NAME_MAX];
        snprintf(reason, sizeof reason,
                 "the guest tree never stopped forking: a new process was adopted in every one of the last "
                 "%d ms, so no enumeration ever found the tree quiescent and the set of members cannot be "
                 "closed; %d peer(s) were enumerated and %d exempted",
                 CKPT_RENDEZVOUS_CHURN_PASSES * 10, nfoll, nexempt);
        ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_PEER_QUIESCENCE, reason);
    }
    if (ndone != nfoll) {
        // Name every participant still outstanding at the rendezvous deadline: "the group never committed"
        // is otherwise indistinguishable from "nothing was ever asked to commit". The host is told about
        // the first one by name, because a reason it cannot act on is barely better than no reason.
        char reason[HL_CKPT_STREAM_NAME_MAX];
        int named = 0;
        for (int i = 0; i < nfoll; i++)
            if (!completed[i]) {
                fprintf(stderr,
                        "[ckpt] participant %lld never committed proc.%d and stopped making progress "
                        "towards a checkpoint safepoint (no host CPU time consumed for %d ms); refusing "
                        "incomplete manifest\n",
                        (long long)foll[i].identity, ckpt_peer_gpid(foll[i].identity),
                        CKPT_RENDEZVOUS_STALL_PASSES * 10);
                if (!named) {
                    named = 1;
                    snprintf(reason, sizeof reason,
                             "%d of %d participants never committed their group; the first is process %lld "
                             "(proc.%d), which stopped making progress towards a checkpoint safepoint -- it "
                             "consumed no host CPU time for %d ms -- or had its dump refused",
                             nfoll - ndone, nfoll, (long long)foll[i].identity, ckpt_peer_gpid(foll[i].identity),
                             CKPT_RENDEZVOUS_STALL_PASSES * 10);
                }
            }
        if (!named) snprintf(reason, sizeof reason, "a participant never committed its group");
        ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_PEER_QUIESCENCE, reason);
    }
    ckpt_phase_finish(&phases, "peer_quiescence", phase, 0);

    // Dump ourselves (the init) last. The statuses the freeze consumed go into THIS group: waitpid(-1)
    // reaps only this process's own children, and an orphan reparents to this process, so the coordinator is
    // the parent of every corpse it collected -- by construction, on both ISAs.
    ckpt_reaped_drop_captured_members(sink);
    phase = ckpt_phase_begin(&phases);
    if (ckpt_dump_self(c, "proc.1", 0) != 0)
        ckpt_coordinator_refuse(&phases, CKPT_REFUSAL_SELF_DUMP,
                                "the container init's own dump failed; the checkpoint would be incomplete");
    ckpt_phase_finish(&phases, "serialization", phase, 0);

    ckpt_publish_manifest(&phases, sink, nfoll, nexempt);
}
