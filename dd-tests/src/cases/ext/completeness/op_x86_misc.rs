use super::*;

/// Misc instruction corners: MOVBE, CMPXCHG16B, RDTSC/RDTSCP, x87, REP string ops, non-temporal stores.
pub(super) fn op_x86_misc() -> Group {
    group(
        "comp-x86-misc",
        vec![
            x("movbe", "completeness/x86_movbe.c"), // jit86 UNIMPL 0F 38 F0 (MOVBE)
            // int3 (#BP -> SIGTRAP) + ud2 (#UD -> SIGILL) reach the guest handler and RESUME. Regression for
            // the Apple-Silicon Mach-exception gap where a JIT'd BRK/UDF killed the process (exit 133/132)
            // instead of delivering the guest signal; now routed through the dispatcher (R_TRAP).
            x("trap-signals", "completeness/x86_trap_signals.c"),
            // MMX movq (plain 0F 6F/7F) is 64-bit; the 128-bit XMM store path corrupted the 8 bytes after
            // the destination. Regression: the sentinel bytes past a 64-bit MMX store must stay intact.
            x("mmx-width", "completeness/x86_mmx_width.c"),
            x("cmpxchg16b", "completeness/x86_cmpxchg16b.c"), // jit86 UNIMPL 0F C7 /1 (CMPXCHG16B)
            x("rdtsc", "completeness/x86_rdtsc.c"), // jit86 UNIMPL 0F 01 F9 (RDTSCP); rdtsc(0F31) ok
            x("x87", "completeness/x86_x87.c"),
            x("repstring", "completeness/x86_repstring.c"), // rep movs/stos/cmps — handled
            x("movnt", "completeness/x86_movnt.c"),         // jit86 UNIMPL 0F E7 (MOVNTDQ)
            x("sbb-acc-imm", "completeness/x86_sbb.c"), // acc-imm SBB 0x1C/0x1D (+REX.W) — result+flags vs qemu
            x("div", "completeness/x86_div.c"), // DIV/IDIV (F6/F7 /6 /7) 8/16/32/64-bit + #DE (SIGFPE) traps
            x("memshift-cl", "completeness/x86_memshift.c"), // D3 /4,/5,/7 mem-dest SHL/SHR/SAR by CL — EA-clobber regression (redis jemalloc)
            x("movntps", "completeness/x86_movntps.c"),      // MOVNTPS/MOVNTPD 0F 2B + MOVNTDQ
            x("movseg", "completeness/x86_movseg.c"), // MOV r/m,Sreg (8C) / MOV Sreg,r/m (8E)
            x("rcl", "completeness/x86_rcl.c"), // RCL/RCR by CL (group2 D2/D3 /2,/3), all widths + mem
            x("pfaf", "completeness/x86_pfaf.c"), // lazy PF/AF dead-elim: live/dead/block vs qemu
            x("flags", "completeness/x86_flags.c"), // shift/rotate/imul/mul DEFINED flags (CF/OF/SF/ZF/PF) all widths
            x("x87b", "completeness/x86_x87b.c"), // FCOMI/FUCOMI, FDIV/FDIVR/FSUBR/FSCALE/FPREM/FIDIV/FXTRACT
            x("ntload", "completeness/x86_ntload.c"), // MOVNTDQA (66 0F38 2A) + MASKMOVDQU (66 0F F7)
            x("vdsoclk", "completeness/x86_vdsoclk.c"), // vDSO clock_gettime ns scaling (monotonic sleep window)
            // x86-xflags: cross-block dead-flag elimination (NZCV + PF/AF liveness across direct edges).
            // The same guest runs in four engine configs so every emission path is oracle-diffed:
            // default (stitch), NOSTITCH=1 (every direct edge chained -> flags_edge/jcc_edge_flags chain
            // paths), NOXBLOCKFLAGS=1 (cross-block pass off -> must equal the old behavior), NOLAZY=1
            // (whole lazy model off). Plus the parity-consumer boundary probe and the SIGALRM timing case.
            x("xflags", "completeness/x86_xflags.c"),
            x("xflags-nostitch", "completeness/x86_xflags.c").env("NOSTITCH", "1"),
            x("xflags-off", "completeness/x86_xflags.c").env("NOXBLOCKFLAGS", "1"),
            x("xflags-nolazy", "completeness/x86_xflags.c").env("NOLAZY", "1"),
            x("parity-edge", "completeness/x86_parity_edge.c"),
            // by-CL (variable-count) shift/rotate flag materialization across a block boundary. The
            // producer runs SHL/SHR/SAR/ROL/ROR/RCL/RCR by %cl (all widths, counts {0,1,2,7,8,31,32,63,64,65})
            // and the flags are read (pushfq) in a SEPARATE block reached by a backward jmp (chained edge). SAR
            // by CL left the live ARM NZCV stale -> a chained-edge spill wrote CF=1/ZF=0 for `sarq %cl` a=0 cl=1;
            // also fixes byte/word count masking (CL&0x1f) and byte/word rotate CF (flags-affected uses CL&0x1f,
            // not CL%width). Run default (backward-jmp chain) AND NOSTITCH=1 (every edge chained) vs qemu.
            x("bycl", "completeness/x86_bycl.c"),
            x("bycl-nostitch", "completeness/x86_bycl.c").env("NOSTITCH", "1"),
            x("xflags-sig", "completeness/x86_xflags_sig.c"),
            x("xflags-sig-nostitch", "completeness/x86_xflags_sig.c").env("NOSTITCH", "1"),
            // Shift/rotate dead-flag elision: an immediate SHL/SHR/SAR whose flag output is dead at every
            // successor (incl. across chained/stitched edges) skips its eager nzcv/PF synthesis. Run default
            // (elision on) AND with the elision off (NOSHIFTFLAGELIDE) + NOSTITCH (force chained edges) +
            // NOLAZY — all four must be byte-identical vs qemu (a wrong elide or wrong keep diverges).
            x("shflag", "completeness/x86_shflag.c"),
            x("shflag-off", "completeness/x86_shflag.c").env("NOSHIFTFLAGELIDE", "1"),
            x("shflag-nostitch", "completeness/x86_shflag.c").env("NOSTITCH", "1"),
            x("shflag-nolazy", "completeness/x86_shflag.c").env("NOLAZY", "1"),
        ],
    )
}
