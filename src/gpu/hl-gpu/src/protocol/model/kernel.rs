//! The **neutral** kernel-IR types + the shader-payload classification magics.
//!
//! This module holds TYPES ONLY — the compiled kernel-IR value types ([`KernelProgram`], [`Inst`], …) and
//! the [`KernelDescriptor`] carried over the `CreateShader` shader channel. There is no PTX parser and no
//! interpreter here; those belong to a driver adapter (`hl-cuda`) and the CPU executor respectively.
//!
//! Crucially, [`KERNEL_MAGIC`] and [`SPIRV_MAGIC`] are defined HERE, in the neutral protocol. The decoder
//! classifies a shader-word payload by these magics ([`crate::protocol::codec::decode`]) — it never
//! reaches into a CUDA/PTX constant. That is the seam that breaks the old ptx leak: the protocol names the
//! payload origins itself, and the CUDA specifics stay in the driver.

/// SPIR-V's canonical leading magic word — a shader payload starting with this is SPIR-V.
pub const SPIRV_MAGIC: u32 = 0x0723_0203;

/// Magic leading word marking `CreateShader.spirv` words as a hl-GPU **kernel descriptor** (kernel source
/// text + launch config) rather than SPIR-V. A software/oracle backend compiles it; a Metal/Vulkan backend
/// would instead carry real SPIR-V. This is the honest, neutral per-backend shader-ABI seam. Its value is
/// a fixed wire constant (compatible with the shipping `hl-gpu`).
pub const KERNEL_MAGIC: u32 = 0xDD6B_0001;

/// Magic leading word marking `CreateShader.spirv` words as a **GLSL descriptor** ([`GlslDescriptor`]):
/// a shader STAGE + entry-point + GLSL source the guest GLES/GL driver forwards VERBATIM for the host to
/// compile (naga's `glsl-in` on the wgpu path). This is the graphics analogue of [`KERNEL_MAGIC`] — the
/// driver ships source, the host owns the compiler — so a driver never has to pre-translate to a
/// backend-specific IR (MSL) the executor cannot consume. Distinct from [`SPIRV_MAGIC`] (`0x07230203`) and
/// [`KERNEL_MAGIC`] (`0xDD6B0001`); `0x67` is ASCII `g` (glsl). Added at `WIRE_VERSION` 6.
pub const GLSL_MAGIC: u32 = 0xDD67_0001;

/// GLSL shader-stage codes carried in a [`GlslDescriptor`] (kept neutral — the protocol never depends on a
/// backend's stage enum such as `naga::ShaderStage`).
pub mod glsl_stage {
    pub const VERTEX: u32 = 0;
    pub const FRAGMENT: u32 = 1;
    pub const COMPUTE: u32 = 2;
}

/// The guest-forwarded GLSL shader: the shader stage ([`glsl_stage`]), the entry-point name the pipeline's
/// `ShaderRef` binds, and the GLSL source the host compiles. Serialized to/from `CreateShader` shader words
/// (led by [`GLSL_MAGIC`]) by [`crate::protocol::codec`] — the graphics counterpart of [`KernelDescriptor`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GlslDescriptor {
    /// Shader stage ([`glsl_stage::VERTEX`] / [`glsl_stage::FRAGMENT`] / [`glsl_stage::COMPUTE`]).
    pub stage: u32,
    /// Entry-point name the render/compute pipeline's `ShaderRef` selects (e.g. `vmain`/`fmain`/`cmain`).
    pub entry: String,
    /// GLSL source text the host compiles (naga `glsl-in` on the wgpu executor).
    pub source: String,
}

/// The guest-forwarded kernel descriptor: the kernel source text, the entry point, and the launch block
/// dims. The host compiles source → [`KernelProgram`]. Serialized to/from `CreateShader` shader words by
/// [`crate::protocol::codec`].
#[derive(Clone, PartialEq, Debug)]
pub struct KernelDescriptor {
    /// Kernel source text (PTX for the CUDA driver; opaque to the protocol).
    pub ptx: String,
    pub entry: String,
    pub block: [u32; 3],
}

// ===================================================================================================
// compiled kernel IR (neutral value types)
// ===================================================================================================

/// Scalar/pointer type tag for a `ld.global`/`st.global` access.
pub mod gty {
    pub const F32: u8 = 0;
    pub const U32: u8 = 1;
    pub const U64: u8 = 2;
}

/// One kernel parameter, in ABI order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Param {
    /// Byte width in the flat parameter blob (4 for u32/f32, 8 for u64).
    pub width: u32,
    /// Byte offset of this parameter within the flat parameter blob (natural alignment).
    pub offset: u32,
    /// True if this parameter is a device pointer (reaches a global memory access).
    pub is_ptr: bool,
    /// Dense storage-binding index among pointer parameters (only meaningful when `is_ptr`).
    pub region: u32,
}

/// An instruction operand: an interned register, or an immediate.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Op {
    Reg(u16),
    ImmI(i64),
    ImmF(u32), // f32 bits
}

/// One compiled instruction over an interned register file.
#[derive(Clone, PartialEq, Debug)]
pub enum Inst {
    MovImmI {
        d: u16,
        imm: u64,
    },
    MovImmF {
        d: u16,
        bits: u32,
    },
    MovReg {
        d: u16,
        s: u16,
    },
    /// Read a special register into `d`. `sreg` is one of the `SR_*` constants.
    MovSReg {
        d: u16,
        sreg: u8,
    },
    LdParam {
        d: u16,
        param: u16,
    },
    Cvta {
        d: u16,
        s: u16,
    },
    /// Integer add. Pointer-aware: `Ptr + int → Ptr` with offset advanced. `wide` keeps 64 bits.
    IAdd {
        d: u16,
        a: Op,
        b: Op,
        wide: bool,
    },
    ISub {
        d: u16,
        a: Op,
        b: Op,
        wide: bool,
    },
    /// `d = a*b + c` (low 32 bits).
    IMad {
        d: u16,
        a: Op,
        b: Op,
        c: Op,
    },
    /// Integer multiply. `wide` = 32×32→64; else low 32. `unsigned` selects zero- vs sign-extension.
    IMul {
        d: u16,
        a: Op,
        b: Op,
        wide: bool,
        unsigned: bool,
    },
    /// Set predicate `d` = (a `cmp` b). `cmp` is a `CMP_*` constant; `unsigned` picks u32 vs s32.
    Setp {
        d: u16,
        a: Op,
        b: Op,
        cmp: u8,
        unsigned: bool,
    },
    /// Branch to instruction index `target`, optionally guarded by predicate reg (negated if `.1`).
    Bra {
        target: u32,
        pred: Option<(u16, bool)>,
    },
    LdGlobal {
        d: u16,
        addr: u16,
        off: i64,
        ty: u8,
    },
    StGlobal {
        addr: u16,
        off: i64,
        src: Op,
        ty: u8,
    },
    /// `ld.shared` — read a 32-bit word from workgroup shared memory.
    LdShared {
        d: u16,
        base: Op,
        off: i64,
        ty: u8,
    },
    /// `st.shared` — write a 32-bit word to workgroup shared memory.
    StShared {
        base: Op,
        off: i64,
        src: Op,
        ty: u8,
    },
    /// `atom.global.<op>` / `red.global.<op>` — atomic read-modify-write on a pointer region.
    AtomGlobal {
        d: Option<u16>,
        addr: u16,
        off: i64,
        op: u8,
        cmp: Op,
        val: Op,
        unsigned: bool,
    },
    /// `atom.shared.<op>` / `red.shared.<op>` — atomic read-modify-write on workgroup shared memory.
    AtomShared {
        d: Option<u16>,
        base: Op,
        off: i64,
        op: u8,
        cmp: Op,
        val: Op,
        unsigned: bool,
    },
    /// Logical/arithmetic shift. `dir` is `SHIFT_*`; `unsigned` selects logical vs arithmetic right shift.
    Shift {
        d: u16,
        a: Op,
        b: Op,
        dir: u8,
        unsigned: bool,
    },
    /// Bitwise `and`/`or`/`xor` (`op` is `BIT_*`).
    BitOp {
        d: u16,
        a: Op,
        b: Op,
        op: u8,
    },
    /// `bar.sync` — a workgroup execution+memory barrier.
    Bar,
    FAdd {
        d: u16,
        a: Op,
        b: Op,
    },
    FSub {
        d: u16,
        a: Op,
        b: Op,
    },
    FMul {
        d: u16,
        a: Op,
        b: Op,
    },
    FFma {
        d: u16,
        a: Op,
        b: Op,
        c: Op,
    },
    /// `cvt` conversions we model: see `CVT_*`.
    Cvt {
        d: u16,
        s: Op,
        kind: u8,
    },
    Ret,
    Nop,
}

// special-register ids
pub const SR_TID_X: u8 = 0;
pub const SR_TID_Y: u8 = 1;
pub const SR_TID_Z: u8 = 2;
pub const SR_NTID_X: u8 = 3;
pub const SR_NTID_Y: u8 = 4;
pub const SR_NTID_Z: u8 = 5;
pub const SR_CTAID_X: u8 = 6;
pub const SR_CTAID_Y: u8 = 7;
pub const SR_CTAID_Z: u8 = 8;
pub const SR_NCTAID_X: u8 = 9;
pub const SR_NCTAID_Y: u8 = 10;
pub const SR_NCTAID_Z: u8 = 11;

// comparison ops
pub const CMP_EQ: u8 = 0;
pub const CMP_NE: u8 = 1;
pub const CMP_LT: u8 = 2;
pub const CMP_LE: u8 = 3;
pub const CMP_GT: u8 = 4;
pub const CMP_GE: u8 = 5;

// cvt kinds
pub const CVT_F32_FROM_S32: u8 = 0;
pub const CVT_S64_FROM_S32: u8 = 1;
pub const CVT_S32_FROM_F32: u8 = 2;
pub const CVT_IDENTITY: u8 = 3;

// atomic ops. All operate on 32-bit words.
pub const ATOM_ADD: u8 = 0;
pub const ATOM_MIN: u8 = 1;
pub const ATOM_MAX: u8 = 2;
pub const ATOM_AND: u8 = 3;
pub const ATOM_OR: u8 = 4;
pub const ATOM_XOR: u8 = 5;
pub const ATOM_EXCH: u8 = 6;
pub const ATOM_CAS: u8 = 7;

// bitwise-shift directions
pub const SHIFT_LEFT: u8 = 0;
pub const SHIFT_RIGHT: u8 = 1;

// bitwise binary ops
pub const BIT_AND: u8 = 0;
pub const BIT_OR: u8 = 1;
pub const BIT_XOR: u8 = 2;

/// A compiled kernel: the entry name, threadgroup (block) dims baked in as the WebGPU-style
/// `local_size`, the parameter layout, and the instruction list over `reg_count` registers.
#[derive(Clone, PartialEq, Debug)]
pub struct KernelProgram {
    pub entry: String,
    pub block: [u32; 3],
    pub params: Vec<Param>,
    /// Total size of the flat parameter blob (binding 0).
    pub param_bytes: u32,
    /// Number of pointer regions (storage bindings 1..=num_regions).
    pub num_regions: u32,
    /// Total workgroup shared-memory size in bytes, rounded up to a 4-byte word. `0` if unused.
    pub shared_bytes: u32,
    pub reg_count: u16,
    pub insts: Vec<Inst>,
}
