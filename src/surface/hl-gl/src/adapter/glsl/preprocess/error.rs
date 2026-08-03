/// Why a GLSL ES stage cannot be preprocessed (GLSL ES 1.00 §3.4). Every variant carries the 1-based source
/// line so `glGetShaderInfoLog`/`glGetProgramInfoLog` can point the application at the offending directive.
/// A stage that produces one of these is REJECTED — an unexpanded macro must never reach the host compiler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreprocessError {
    /// A `#` directive this preprocessor does not implement (GLSL ES 1.00 §3.4 lists the whole set).
    UnknownDirective { line: usize, name: String },
    /// A recognized directive has invalid syntax, value, or placement.
    InvalidDirective { line: usize, name: String },
    /// A block comment reaches the end of the compilation unit without `*/`.
    UnterminatedComment { line: usize },
    /// A missing, reserved, or non-identifier macro name in `#define`/`#undef`.
    MacroName { line: usize, name: String },
    /// A malformed function-like `#define` parameter list.
    MacroParameters { line: usize, name: String },
    /// A macro was redefined with a different parameter or replacement-token sequence.
    MacroRedefinition { line: usize, name: String },
    /// A function-like macro invoked with the wrong number of arguments.
    MacroArguments {
        line: usize,
        name: String,
        expected: usize,
        found: usize,
    },
    /// A function-like macro whose argument list does not close on the same logical line.
    MacroInvocation { line: usize, name: String },
    /// `#`/`##` in a replacement list. GLSL ES has no stringize or token-paste operator.
    TokenPaste { line: usize, name: String },
    /// Macro expansion exceeded [`super::MAX_EXPANSION_DEPTH`].
    MacroDepth { line: usize, name: String },
    /// A `#if`/`#elif` controlling expression that is not an integral constant expression.
    Condition { line: usize, expression: String },
    /// `#else`/`#elif`/`#endif` with no open `#if`, or `#elif` after `#else`.
    ConditionalNesting { line: usize, directive: String },
    /// End of source reached with a `#if` still open.
    UnterminatedConditional { line: usize },
    /// The shader asked to be rejected with `#error`.
    Error { line: usize, message: String },
}

impl std::fmt::Display for PreprocessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownDirective { line, name } => {
                write!(f, "{line}: unsupported preprocessor directive `#{name}`")
            }
            Self::InvalidDirective { line, name } => {
                write!(f, "{line}: invalid `#{name}` directive")
            }
            Self::UnterminatedComment { line } => {
                write!(f, "{line}: unterminated block comment")
            }
            Self::MacroName { line, name } => {
                write!(f, "{line}: invalid or reserved macro name `{name}`")
            }
            Self::MacroParameters { line, name } => {
                write!(f, "{line}: malformed parameter list for macro `{name}`")
            }
            Self::MacroRedefinition { line, name } => {
                write!(f, "{line}: incompatible redefinition of macro `{name}`")
            }
            Self::MacroArguments {
                line,
                name,
                expected,
                found,
            } => write!(
                f,
                "{line}: macro `{name}` expects {expected} argument(s) but {found} were given"
            ),
            Self::MacroInvocation { line, name } => write!(
                f,
                "{line}: macro `{name}` needs an argument list closing on the same line"
            ),
            Self::TokenPaste { line, name } => write!(
                f,
                "{line}: macro `{name}` uses `#`/`##`, which GLSL ES does not define"
            ),
            Self::MacroDepth { line, name } => {
                write!(f, "{line}: macro `{name}` expands recursively")
            }
            Self::Condition { line, expression } => write!(
                f,
                "{line}: `{expression}` is not an integral constant expression"
            ),
            Self::ConditionalNesting { line, directive } => {
                write!(f, "{line}: `#{directive}` without a matching `#if`")
            }
            Self::UnterminatedConditional { line } => {
                write!(f, "{line}: `#if` without a matching `#endif`")
            }
            Self::Error { line, message } => write!(f, "{line}: #error {message}"),
        }
    }
}

impl std::error::Error for PreprocessError {}
