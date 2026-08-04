use hl_task::{AlternateStack, SignalMask};

use super::frame::{
    FrameContext, FrameError, FrameImage, FrameReader, FrameRequest, Machine, X86_SIGNAL_FRAME_SIZE, X86SignalMachine,
    alternate_stack, put_i32, put_u32, put_u64, put_u128,
};

const RETURN: usize = 0;
const UCONTEXT: usize = 8;
const MCONTEXT: usize = UCONTEXT + 40;
const SIGINFO: usize = UCONTEXT + 512;
const XMM: usize = UCONTEXT + 768;
const YMMH: usize = XMM + 16 * 16;
const SA_ONSTACK: u64 = 0x0800_0000;
const SA_NODEFER: u64 = 0x4000_0000;
const GREG_TO_REGISTER: [usize; 16] = [8, 9, 10, 11, 12, 13, 14, 15, 7, 6, 5, 3, 2, 0, 1, 4];
const _: () = assert!(YMMH + 16 * 16 <= X86_SIGNAL_FRAME_SIZE);

pub(crate) struct X86FrameCodec;

impl X86FrameCodec {
    pub(crate) fn build(request: &FrameRequest, machine: &X86SignalMachine) -> Result<FrameImage, FrameError> {
        let (base, stack, handler_stack) = Self::stack_base(machine.stack_pointer, request)?;
        let ucontext = base
            .checked_sub((X86_SIGNAL_FRAME_SIZE - 8) as u64)
            .ok_or(FrameError::Address)?
            & !15;
        let write_address = ucontext.checked_sub(8).ok_or(FrameError::Address)?;
        let mut bytes = vec![0; X86_SIGNAL_FRAME_SIZE];
        put_u64(&mut bytes, RETURN, request.sigreturn_pc);
        Self::encode_stack(&mut bytes, stack);
        for (greg, register) in GREG_TO_REGISTER.iter().enumerate() {
            put_u64(&mut bytes, MCONTEXT + greg * 8, machine.registers[*register]);
        }
        put_u64(&mut bytes, MCONTEXT + 16 * 8, machine.instruction_pointer);
        put_u64(&mut bytes, MCONTEXT + 17 * 8, machine.rflags | 2);
        put_u64(&mut bytes, UCONTEXT + 296, request.mask.bits());
        for (index, value) in machine.vectors.iter().enumerate() {
            put_u128(&mut bytes, XMM + index * 16, *value);
        }
        for (index, value) in machine.vector_upper.iter().enumerate() {
            put_u128(&mut bytes, YMMH + index * 16, *value);
        }
        Self::encode_info(&mut bytes, request);
        let mut handler = machine.clone();
        handler.registers[7] = u64::from(request.information.signal.get());
        handler.registers[6] = write_address + SIGINFO as u64;
        handler.registers[2] = ucontext;
        handler.registers[4] = write_address;
        handler.stack_pointer = write_address;
        handler.instruction_pointer = match request.action.disposition {
            hl_task::SignalDisposition::Handler(handler) => handler,
            _ => return Err(FrameError::Malformed),
        };
        let mut mask = request.mask.bits() | request.action.mask.bits();
        if request.action.flags & SA_NODEFER == 0 {
            mask |= 1_u64 << (request.information.signal.get() - 1);
        }
        Ok(FrameImage {
            write_address,
            bytes,
            handler_machine: Machine::X86_64(handler),
            handler_mask: SignalMask::from_bits(mask),
            handler_alternate_stack: handler_stack,
        })
    }

    pub(crate) fn restore(ucontext: u64, bytes: &[u8]) -> Result<FrameContext, FrameError> {
        if ucontext & 15 != 0 {
            return Err(FrameError::Alignment);
        }
        if bytes.len() != X86_SIGNAL_FRAME_SIZE - 8 {
            return Err(FrameError::Malformed);
        }
        let mut framed = vec![0; X86_SIGNAL_FRAME_SIZE];
        framed[8..].copy_from_slice(bytes);
        let mut registers = [0; 16];
        for (greg, register) in GREG_TO_REGISTER.iter().enumerate() {
            registers[*register] = FrameReader::u64(&framed, MCONTEXT + greg * 8);
        }
        let stack_pointer = registers[4];
        let instruction_pointer = FrameReader::u64(&framed, MCONTEXT + 16 * 8);
        let rflags = FrameReader::u64(&framed, MCONTEXT + 17 * 8);
        let segments = FrameReader::u64(&framed, MCONTEXT + 18 * 8);
        if !Self::canonical(stack_pointer)
            || !Self::canonical(instruction_pointer)
            || rflags & 2 == 0
            || rflags & !0x0000_0000_0024_0fd7 != 0
            || segments != 0
        {
            return Err(FrameError::UnsupportedState);
        }
        let mut vectors = [0; 16];
        for (index, value) in vectors.iter_mut().enumerate() {
            *value = FrameReader::u128(&framed, XMM + index * 16);
        }
        let mut vector_upper = [0; 16];
        for (index, value) in vector_upper.iter_mut().enumerate() {
            *value = FrameReader::u128(&framed, YMMH + index * 16);
        }
        Ok(FrameContext {
            machine: Machine::X86_64(X86SignalMachine {
                registers,
                vectors,
                vector_upper,
                stack_pointer,
                instruction_pointer,
                rflags,
            }),
            mask: SignalMask::from_bits(FrameReader::u64(&framed, UCONTEXT + 296)),
            alternate_stack: alternate_stack(
                FrameReader::u64(&framed, UCONTEXT + 16),
                FrameReader::u32(&framed, UCONTEXT + 24),
                FrameReader::u64(&framed, UCONTEXT + 32),
                stack_pointer,
            )?,
        })
    }

    fn canonical(address: u64) -> bool {
        let upper = address >> 48;
        upper == 0 || upper == 0xffff
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
