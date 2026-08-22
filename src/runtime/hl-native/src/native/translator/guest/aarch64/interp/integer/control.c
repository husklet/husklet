// MRS/MSR project architectural registers onto the engine's CPU model and per-thread state.
static int interp_exec_system_register(struct cpu *cpu, uint32_t insn, uint64_t gpc) {
    int rt = (int)(insn & 31);
    uint32_t reg = insn & 0xFFFFFFE0u;
    int is_read = (insn & 0x00200000u) != 0; // bit 21 is L: 1 = MRS, 0 = MSR

    // HWCAP_CPUID is clear, so an EL1 ID-register access is architecturally undefined at EL0.
    if (is_read && (insn & 0xFFFF0000u) == 0xD5380000u && !g_aarch64_cpu_model.user_id_registers) {
        cpu->pc = gpc;
        interp_raise_sync_signal(cpu, 4 /* SIGILL */, 1 /* ILL_ILLOPC */, pcrel_base(gpc));
        return INTERP_END;
    }
    switch (reg) {
    case 0xD53B0020u: // MRS CTR_EL0
        interp_set_gpr(cpu, rt, g_aarch64_cpu_model.ctr_el0);
        break;
    case 0xD53B00E0u: // MRS DCZID_EL0
        interp_set_gpr(cpu, rt, g_aarch64_cpu_model.dczid_el0);
        break;
    case 0xD53BD040u: // MRS TPIDR_EL0
    case 0xD53BD060u: // MRS TPIDRRO_EL0
        interp_set_gpr(cpu, rt, cpu->tls);
        break;
    case 0xD51BD040u: // MSR TPIDR_EL0
        cpu->tls = interp_gpr(cpu, rt);
        break;
    case 0xD53B4200u: // MRS NZCV
        interp_set_gpr(cpu, rt, cpu->nzcv & (INTERP_NZCV_N | INTERP_NZCV_Z | INTERP_NZCV_C | INTERP_NZCV_V));
        break;
    case 0xD51B4200u: // MSR NZCV
        cpu->nzcv = interp_gpr(cpu, rt) & (INTERP_NZCV_N | INTERP_NZCV_Z | INTERP_NZCV_C | INTERP_NZCV_V);
        break;
    case 0xD53B4220u: // MRS DAIF
        interp_set_gpr(cpu, rt, 0);
        break;
    case 0xD51B4220u: // MSR DAIF
        break;
    case 0xD53B4400u: // MRS FPCR
        interp_set_gpr(cpu, rt, g_interp_fpcr);
        break;
    case 0xD51B4400u: // MSR FPCR
        g_interp_fpcr = interp_gpr(cpu, rt) & INTERP_FPCR_WRITABLE;
        break;
    case 0xD53B4420u: // MRS FPSR
        interp_set_gpr(cpu, rt, g_interp_fpsr);
        break;
    case 0xD51B4420u: // MSR FPSR
        g_interp_fpsr = interp_gpr(cpu, rt) & INTERP_FPSR_WRITABLE;
        break;
    case 0xD53BE000u: // MRS CNTFRQ_EL0
        interp_set_gpr(cpu, rt, UINT64_C(1000000000));
        break;
    case 0xD53BE020u: // MRS CNTPCT_EL0
    case 0xD53BE040u: // MRS CNTVCT_EL0
    case 0xD53BE0C0u: // MRS CNTVCTSS_EL0
        interp_set_gpr(cpu, rt, now_ns());
        break;
    default: return interp_undefined(cpu, insn, "system -- unmodelled system register (MRS/MSR)");
    }
    cpu->pc = gpc + 4;
    return INTERP_NEXT;
}

// Branches, exception generating and system instructions. Every form here ends the block.
static int interp_exec_branch_system(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;

    if ((insn & 0x7C000000u) == 0x14000000u) {
        int64_t offset = interp_sext(insn & 0x3FFFFFFu, 26) << 2;
        if (insn & 0x80000000u) {
            // pcrel_base: the guest's own view of the return address, un-biased for a non-PIE image.
            cpu->x[30] = pcrel_base(gpc) + 4;
        }
        cpu->pc = gpc + (uint64_t)offset;
        cpu->reason = R_BRANCH;
        return INTERP_END;
    }

    // B.cond, and BC.cond (the v8.8 hint: bit 4 apart, same architectural effect).
    if ((insn & 0xFF000010u) == 0x54000000u || (insn & 0xFF000010u) == 0x54000010u) {
        int64_t offset = interp_sext((insn >> 5) & 0x7FFFFu, 19) << 2;
        cpu->pc = interp_cond_holds(cpu, insn & 0xFu) ? gpc + (uint64_t)offset : gpc + 4;
        cpu->reason = R_BRANCH;
        return INTERP_END;
    }

    if ((insn & 0x7E000000u) == 0x34000000u) {
        unsigned sf = (insn >> 31) & 1, nonzero = (insn >> 24) & 1;
        int64_t offset = interp_sext((insn >> 5) & 0x7FFFFu, 19) << 2;
        uint64_t value = interp_gpr(cpu, (int)(insn & 31));
        int is_zero = sf ? value == 0 : (uint32_t)value == 0;
        cpu->pc = (nonzero ? !is_zero : is_zero) ? gpc + (uint64_t)offset : gpc + 4;
        cpu->reason = R_BRANCH;
        return INTERP_END;
    }

    // TBZ / TBNZ: the bit position is b5:b40, so insn[31] is its high bit and not an sf field.
    if ((insn & 0x7E000000u) == 0x36000000u) {
        unsigned nonzero = (insn >> 24) & 1;
        unsigned bit = (unsigned)(((insn >> 31) & 1) << 5) | ((insn >> 19) & 0x1Fu);
        int64_t offset = interp_sext((insn >> 5) & 0x3FFFu, 14) << 2;
        uint64_t value = interp_gpr(cpu, (int)(insn & 31));
        int set = (int)((value >> bit) & 1);
        cpu->pc = (nonzero ? set : !set) ? gpc + (uint64_t)offset : gpc + 4;
        cpu->reason = R_BRANCH;
        return INTERP_END;
    }

    // BR / BLR / RET (the PAC/ERET forms are not modelled).
    if ((insn & 0xFE000000u) == 0xD6000000u) {
        unsigned opc = (insn >> 21) & 0xFu, op2 = (insn >> 16) & 0x1Fu, op3 = (insn >> 10) & 0x3Fu;
        int rn = (int)((insn >> 5) & 31);
        unsigned op4 = insn & 0x1Fu;
        if (op2 != 0x1F || op3 != 0 || op4 != 0)
            return interp_undefined(cpu, insn, "branch register -- pointer-authenticated branch (BRAA/BLRAA/RETAA)");
        switch (opc) {
        case 0: // BR
            cpu->pc = interp_gpr(cpu, rn);
            break;
        case 1: // BLR
        {
            uint64_t target = interp_gpr(cpu, rn);
            cpu->x[30] = pcrel_base(gpc) + 4;
            cpu->pc = target; // read the target BEFORE writing x30: `blr x30` must use the old value
            break;
        }
        case 2: // RET
            cpu->pc = interp_gpr(cpu, rn);
            break;
        default: return interp_undefined(cpu, insn, "branch register -- ERET/DRPS or unallocated opc");
        }
        cpu->reason = R_BRANCH;
        return INTERP_END;
    }

    if ((insn & 0xFF000000u) == 0xD4000000u) {
        unsigned opc = (insn >> 21) & 7, ll = insn & 3u;
        if (opc == 0 && ll == 1) {
            // The PC stays ON the svc; the dispatcher advances it unless the syscall set pc itself.
            cpu->pc = gpc;
            cpu->reason = R_SYSCALL;
            return INTERP_END;
        }
        // A GUEST event, not an engine gap: this must not reach interp_undefined (fatal, exit 70). BRK is
        // SIGTRAP/TRAP_BRKPT with the PC left ON it, so a handler that returns re-executes; HLT, HVC/SMC and
        // DCPS are UNDEFINED at EL0, so SIGILL.
        int signo = opc == 1 ? 5 /* SIGTRAP */ : 4 /* SIGILL */;
        /* TRAP_BRKPT and ILL_ILLOPC are both 1, so selecting between them with a ternary is
         * `-Werror=duplicated-branches`: two arms GCC can see are identical. The value carries
         * both meanings and is named rather than chosen. Same shape as ENOATTR/ENODATA. */
        int signal_code = 1; // TRAP_BRKPT when signo is SIGTRAP, ILL_ILLOPC when it is SIGILL
        cpu->pc = gpc;       // the faulting instruction the guest's frame must name
        // si_addr is pcrel_base(gpc): signal_canonicalize_pc treats the frame's own pc the same way.
        interp_raise_sync_signal(cpu, signo, signal_code, pcrel_base(gpc));
        return INTERP_END;
    }

    // Hints (the NOP space). Every member is a no-op at EL0; taking the whole space covers later hints.
    if ((insn & 0xFFFFF01Fu) == 0xD503201Fu) {
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // Barriers. Rt is pinned to 11111: the constant is 0xD503301F, and 0xD5033000 would catch no barrier.
    if ((insn & 0xFFFFF01Fu) == 0xD503301Fu) {
        unsigned op2 = (insn >> 5) & 7;
        if (op2 == 6) {
            // ISB commits the icache dance: exit R_ICCOMMIT so smc_commit() drops rewritten blocks. New
            // BYTES come free from re-decoding, but the cached block EXTENT came from the old ones.
            cpu->pc = gpc + 4;
            cpu->reason = R_ICCOMMIT;
            return INTERP_END;
        }
        // DSB / DMB / SB / CLREX. Guest threads are host threads and guest accesses are ordinary C accesses
        // the host may reorder, so a guest barrier needs a real host one; SEQ_CST covers every ordering here.
        __atomic_thread_fence(__ATOMIC_SEQ_CST);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if ((insn & 0xFFFFFFE0u) == 0xD50B7B20u) { // dc cvau, Xt
        // No-op: the host never instruction-fetches guest pages.
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    if ((insn & 0xFFFFFFE0u) == 0xD50B7520u) { // ic ivau, Xt
        // Record the line and exit R_ICFLUSH; smc_icflush() only QUEUES it, the drop happens at the ISB.
        cpu->smc_va = interp_gpr(cpu, (int)(insn & 31));
        cpu->pc = gpc + 4;
        cpu->reason = R_ICFLUSH;
        return INTERP_END;
    }

    // DC ZVA zeroes the block size advertised in DCZID_EL0 (== 4, so 64 bytes), never the host's.
    if ((insn & 0xFFFFFFE0u) == 0xD50B7420u) {
        uint64_t address = interp_gpr(cpu, (int)(insn & 31)) & ~UINT64_C(63);
        for (unsigned offset = 0; offset < 64u; offset += 8)
            interp_store_bits(address + offset, 0, 8);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // MSR (immediate): DAIF and the other PSTATE fields. Interrupt masking is meaningless at EL0.
    // op1 == 000 is excluded: it is CFINV / XAFLAG / AXFLAG (FEAT_FlagM, FlagM2), which REWRITE NZCV, plus
    // the EL1-only UAO/PAN/SPSel. Neither feature is advertised -- as with RMIF and SETF8/16 -- and running
    // them as no-ops left the flags silently wrong.
    if ((insn & 0xFFF8F01Fu) == 0xD500401Fu) {
        unsigned pstate_op1 = (insn >> 16) & 7u, pstate_op2 = (insn >> 5) & 7u;
        // op1:op2 == 011:011 is SVCR -- SMSTART/SMSTOP: no-oping it tells a guest streaming mode is on while
        // op0 == 0001 reports every SME instruction. Report, as the top-level decode already does.
        if (pstate_op1 == 0 || (pstate_op1 == 3 && pstate_op2 == 3))
            return interp_undefined(cpu, insn,
                                    "system -- MSR (immediate) CFINV/XAFLAG/AXFLAG, SMSTART/SMSTOP, or an EL1 "
                                    "PSTATE field");
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // Every value comes from the CPU model or emulated per-thread state, never the host.
    if ((insn & 0xFFD00000u) == 0xD5100000u) return interp_exec_system_register(cpu, insn, gpc);

    if ((insn & 0xFFC00000u) == 0xD5000000u)
        return interp_undefined(cpu, insn, "system -- SYS/SYSL maintenance operation");

    return interp_undefined(cpu, insn, "branches, exception generating and system -- unallocated encoding");
}
