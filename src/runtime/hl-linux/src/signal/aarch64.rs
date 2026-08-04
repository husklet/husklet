use hl_task::{AlternateStack, SignalMask};

use super::frame::{
    AARCH64_SIGNAL_FRAME_SIZE, Aarch64SignalMachine, FrameContext, FrameError, FrameImage, FrameReader, FrameRequest,
    Machine, alternate_stack, put_i32, put_u32, put_u64, put_u128,
};

const SIGINFO: usize = 0;
const UCONTEXT: usize = 128;
const MCONTEXT: usize = UCONTEXT + 176;
const FPSIMD: usize = MCONTEXT + 288;
const FPSIMD_MAGIC: u32 = 0x4650_8001;
const FPSIMD_SIZE: u32 = 528;
const SA_ONSTACK: u64 = 0x0800_0000;
const SA_NODEFER: u64 = 0x4000_0000;
const _: () = assert!(FPSIMD + FPSIMD_SIZE as usize + 8 <= AARCH64_SIGNAL_FRAME_SIZE);

pub(crate) struct Aarch64FrameCodec;

impl Aarch64FrameCodec {
    pub(crate) fn build(request: &FrameRequest, machine: &Aarch64SignalMachine) -> Result<FrameImage, FrameError> {
        let (base, stack, handler_stack) = Self::stack_base(machine.stack_pointer, request)?;
        let frame = base
            .checked_sub(AARCH64_SIGNAL_FRAME_SIZE as u64)
            .ok_or(FrameError::Address)?
            & !15;
        let mut bytes = vec![0; AARCH64_SIGNAL_FRAME_SIZE];
        Self::encode_info(&mut bytes, request);
        Self::encode_stack(&mut bytes, stack);
        put_u64(&mut bytes, UCONTEXT + 40, request.mask.bits());
        put_u64(&mut bytes, MCONTEXT, request.information.address);
        for (index, value) in machine.registers.iter().enumerate() {
            put_u64(&mut bytes, MCONTEXT + 8 + index * 8, *value);
        }
        put_u64(&mut bytes, MCONTEXT + 256, machine.stack_pointer);
        put_u64(&mut bytes, MCONTEXT + 264, machine.program_counter);
        put_u64(&mut bytes, MCONTEXT + 272, machine.pstate);
        put_u32(&mut bytes, FPSIMD, FPSIMD_MAGIC);
        put_u32(&mut bytes, FPSIMD + 4, FPSIMD_SIZE);
        put_u32(&mut bytes, FPSIMD + 8, machine.fpsr);
        put_u32(&mut bytes, FPSIMD + 12, machine.fpcr);
        for (index, value) in machine.vectors.iter().enumerate() {
            put_u128(&mut bytes, FPSIMD + 16 + index * 16, *value);
        }
        let mut handler = machine.clone();
        handler.registers[0] = u64::from(request.information.signal.get());
        handler.registers[1] = frame;
        handler.registers[2] = frame + UCONTEXT as u64;
        handler.registers[30] = request.sigreturn_pc;
        handler.stack_pointer = frame;
        handler.program_counter = match request.action.disposition {
            hl_task::SignalDisposition::Handler(handler) => handler,
            _ => return Err(FrameError::Malformed),
        };
        let mut mask = request.mask.bits() | request.action.mask.bits();
        if request.action.flags & SA_NODEFER == 0 {
            mask |= 1_u64 << (request.information.signal.get() - 1);
        }
        let handler_mask = SignalMask::from_bits(mask);
        Ok(FrameImage {
            write_address: frame,
            bytes,
            handler_machine: Machine::Aarch64(handler),
            handler_mask,
            handler_alternate_stack: handler_stack,
        })
    }

    pub(crate) fn restore(frame: u64, bytes: &[u8]) -> Result<FrameContext, FrameError> {
        if frame & 15 != 0 {
            return Err(FrameError::Alignment);
        }
        if bytes.len() != AARCH64_SIGNAL_FRAME_SIZE {
            return Err(FrameError::Malformed);
        }
        if FrameReader::u32(bytes, FPSIMD) != FPSIMD_MAGIC
            || FrameReader::u32(bytes, FPSIMD + 4) != FPSIMD_SIZE
            || FrameReader::u32(bytes, FPSIMD + FPSIMD_SIZE as usize) != 0
        {
            return Err(FrameError::Malformed);
        }
        let mut registers = [0; 31];
        for (index, value) in registers.iter_mut().enumerate() {
            *value = FrameReader::u64(bytes, MCONTEXT + 8 + index * 8);
        }
        let stack_pointer = FrameReader::u64(bytes, MCONTEXT + 256);
        let program_counter = FrameReader::u64(bytes, MCONTEXT + 264);
        let pstate = FrameReader::u64(bytes, MCONTEXT + 272);
        if stack_pointer & 15 != 0 || program_counter & 3 != 0 || pstate & !0xf000_0000 != 0 {
            return Err(FrameError::UnsupportedState);
        }
        let mut vectors = [0; 32];
        for (index, value) in vectors.iter_mut().enumerate() {
            *value = FrameReader::u128(bytes, FPSIMD + 16 + index * 16);
        }
        Ok(FrameContext {
            machine: Machine::Aarch64(Aarch64SignalMachine {
                registers,
                vectors,
                stack_pointer,
                program_counter,
                pstate,
                fpsr: FrameReader::u32(bytes, FPSIMD + 8),
                fpcr: FrameReader::u32(bytes, FPSIMD + 12),
            }),
            mask: SignalMask::from_bits(FrameReader::u64(bytes, UCONTEXT + 40)),
            alternate_stack: alternate_stack(
                FrameReader::u64(bytes, UCONTEXT + 16),
                FrameReader::u32(bytes, UCONTEXT + 24),
                FrameReader::u64(bytes, UCONTEXT + 32),
                stack_pointer,
            )?,
        })
    }

    fn encode_info(bytes: &mut [u8], request: &FrameRequest) {
        put_i32(bytes, SIGINFO, i32::from(request.information.signal.get()));
        put_i32(bytes, SIGINFO + 4, request.information.error);
        put_i32(bytes, SIGINFO + 8, request.information.code);
        put_u64(bytes, SIGINFO + 16, request.information.address);
        put_u64(bytes, SIGINFO + 24, request.information.value);
        if request.information.signal.get() == 31 && request.information.code == 1 {
            put_u32(bytes, SIGINFO + 28, request.information.source_tag);
        }
        if request.information.sender_process != 0 {
            put_u32(bytes, SIGINFO + 16, request.information.sender_process);
            put_u32(bytes, SIGINFO + 20, request.information.sender_user);
        }
    }

    fn stack_base(current: u64, request: &FrameRequest) -> Result<(u64, AlternateStack, AlternateStack), FrameError> {
        let stack = request.alternate_stack;
        let (pointer, size) = match stack {
            AlternateStack::Disabled => return Ok((current, AlternateStack::Disabled, AlternateStack::Disabled)),
            AlternateStack::Enabled { pointer, size }
            | AlternateStack::Autodisarm { pointer, size }
            | AlternateStack::Active { pointer, size } => (pointer, size),
        };
        let end = pointer.checked_add(size).ok_or(FrameError::Overflow)?;
        let inside = current >= pointer && current < end;
        if matches!(stack, AlternateStack::Enabled { .. }) && request.action.flags & SA_ONSTACK != 0 && !inside {
            Ok((
                end,
                AlternateStack::Active { pointer, size },
                AlternateStack::Active { pointer, size },
            ))
        } else if matches!(stack, AlternateStack::Autodisarm { .. })
            && request.action.flags & SA_ONSTACK != 0
            && !inside
        {
            Ok((end, stack, AlternateStack::Disabled))
        } else {
            Ok((current, stack, stack))
        }
    }

    fn encode_stack(bytes: &mut [u8], stack: AlternateStack) {
        match stack {
            AlternateStack::Disabled => put_u32(bytes, UCONTEXT + 24, 2),
            AlternateStack::Enabled { pointer, size } | AlternateStack::Active { pointer, size } => {
                put_u64(bytes, UCONTEXT + 16, pointer);
                put_u64(bytes, UCONTEXT + 32, size);
                put_u32(
                    bytes,
                    UCONTEXT + 24,
                    u32::from(matches!(stack, AlternateStack::Active { .. })),
                );
            }
            AlternateStack::Autodisarm { pointer, size } => {
                put_u64(bytes, UCONTEXT + 16, pointer);
                put_u64(bytes, UCONTEXT + 32, size);
                put_u32(bytes, UCONTEXT + 24, 0x8000_0000);
            }
        }
    }
}
