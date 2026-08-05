# x86 emitter capacity audit

The retained C engine was inspected at
`../engine/src/translator/guest/x86_64/emit.c` and
`../engine/src/translator/guest/x86_64/translate.c`. Its emitter advances the
shared code-arena cursor directly; it has no 128-word per-vector contract and
therefore provides no justification for that test constant.

The Rust/native production owner is
`src/arch/x86_64/frontend.c::hl_x86_a64_emit`. It computes every instruction's
word count before emission, compares the complete block against the caller's
`host_capacity`, and returns `HL_X86_A64_CAPACITY` without emitting when the
block does not fit. `src/arch/x86_64/run.c::build_translation` supplies an
8,192-word array with explicit entry and relocation reserves. The vector
generator's sizing function and emitter are checked for equality by the native
translation suite.

The failing 128-word limit belonged only to `vector_fragment`, a test helper.
The helper's three callers all own 256-word arrays. Writable-view journal
coalescing increased `HL_X86_WRITE_CACHE_WORDS` from 71 to 105, legitimately
making the generated writable-vector fragment exceed the obsolete test-only
constant while remaining within each caller's real allocation and the
production preflight contract. Removing that stale early return then exposed a
second defect: `emit_write_cache` emits 109 words while
`HL_X86_WRITE_CACHE_WORDS` claimed 105. Production consequently under-reserved
four words for every generated writable-memory operation. The shared sizing
constant is corrected to the emitter's exact size.

Two sizing functions derive their exact count by emitting into bounded stack
scratch. Writable rotate and compare-exchange forms can now exceed their old
256-word scratch after the same cache expansion, so those buffers are raised to
the existing 512-word bound already used by the sibling bit, double-shift, and
exchange sizing paths. These buffers are private sizing workspace, not generated
block capacity.

The generic CALL/control sizing base also embedded the former 71-word writable
cache size. Its base is raised by the same 34-word delta, and the focused control
test supplies enough capacity for the now-larger valid fragment. This restores
the production rule that the sizing pass rejects capacity before any emission.

The correction passes each caller's actual capacity into the helper and
reserves one additional word for its appended return instruction. The generator
still emits identical code; its production capacity preflight now accounts for
the complete sequence.
