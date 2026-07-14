//! Guest language runtimes: managed JIT-in-JIT engines (V8/JVM/LuaJIT/PyPy/YJIT/BEAM/Julia/RyuJIT
//! emitting + executing their own machine code inside the hl JIT), unusual-language interpreters/
//! compilers (Haskell/R/Forth/Tcl/Lua/Perl/awk), and Python C-extensions (zlib/libcrypto/ctypes FFI).

use crate::scenario::{scen, Scenario, Target};

pub(super) fn items() -> Vec<Scenario> {
    vec![
        // ============================ JIT-in-JIT: managed runtimes ============================
        // Each forces the GUEST's own JIT to emit + execute machine code inside the hl JIT.
        // seed (proven on Real): node = a V8 JIT running inside the hl JIT. Deterministic.
        scen("weird/v8-jit-in-jit", "node:alpine")
            .exec("node -e 'let s=0;for(let i=1;i<=1000000;i++)s+=i;console.log(\"sum\",s)'")
            .has("sum 500000500000"),   // sum(1..1e6); corrected from the seed's typo
        // V8 BigInt fib(90) — exercises the BigInt slow path + JIT'd hot loop.
        scen("weird/v8-bigint", "node:alpine")
            .exec("node -e 'let a=0n,b=1n;for(let i=0;i<90;i++){[a,b]=[b,a+b]};console.log(\"FIB=\"+a)'")
            .has("FIB=2880067194370816120"),
        // V8 hot integer loop (prime sieve) — forces TurboFan to compile the inner loop.
        scen("weird/node-primes", "node:alpine")
            .exec("node -e 'let p=0;for(let n=2;n<100000;n++){let q=1;for(let d=2;d*d<=n;d++)if(n%d===0){q=0;break}p+=q}console.log(\"PRIMES=\"+p)'")
            .has("PRIMES=9592"),
        // JVM C2: 50M-iteration loop forces HotSpot C2 to JIT-compile the method (javac then java).
        scen("weird/jvm-c2-jit", "eclipse-temurin:21")
            .exec("cat > /M.java <<'EOF'\npublic class M{public static void main(String[] a){long s=0;for(long i=0;i<50000000L;i++)s+=i%7;System.out.println(\"JVM=\"+s);}}\nEOF\njavac /M.java -d /o && java -cp /o M")
            .has("JVM=149999997"),
        // JVM bigint-ish fib via javac+run (compile + managed JIT).
        scen("weird/jvm-fib", "eclipse-temurin:21")
            .exec("cat > /F.java <<'EOF'\npublic class F{public static void main(String[] a){long x=0,y=1;for(int i=0;i<50;i++){long t=x+y;x=y;y=t;}System.out.println(\"JFIB=\"+x);}}\nEOF\njavac /F.java -d /o && java -cp /o F")
            .has("JFIB=12586269025"),
        // LuaJIT trace compiler: a 10M-iteration loop triggers LuaJIT's tracing JIT to emit native code.
        scen("weird/luajit-trace", "openresty/openresty:alpine")
            .exec("/usr/local/openresty/luajit/bin/luajit -e 'local s=0 for i=1,10000000 do s=s+i%7 end print(\"LUAJIT=\"..s)'")
            .has("LUAJIT=29999997"),
        // PyPy: tracing JIT (RPython) compiles the prime-counting loop.
        scen("weird/pypy-compute", "pypy:3")
            .exec("pypy3 -c \"print('PYPY='+str(sum(1 for n in range(2,30000) if all(n%d for d in range(2,int(n**0.5)+1)))))\"")
            .has("PYPY=3245"),
        // Ruby YJIT: --yjit enables the in-process YJIT compiler on a hot loop.
        scen("weird/ruby-yjit", "ruby:3.3")
            .exec("ruby --yjit -e 's=0; (1..2000000).each{|i| s+=i%7}; puts \"YJIT=#{s}\"'")
            .has("YJIT=5999997"),
        // Ruby MRI bigint fib (interpreter + Bignum path).
        scen("weird/ruby-fib", "ruby:3.3")
            .exec("ruby -e 'a,b=0,1;50.times{a,b=b,a+b};puts \"RB=#{a}\"'")
            .has("RB=12586269025"),
        // Erlang/BEAM: OTP 27 ships BeamAsm, an in-process native-code JIT.
        scen("weird/beam-jit", "erlang:27")
            .exec("erl -noshell -eval 'io:format(\"ERL=~w~n\",[lists:sum(lists:seq(1,1000))]), halt().'")
            .has("ERL=500500").long(),
        // Julia: every function is JIT-compiled through LLVM on first call (in-process codegen).
        scen("weird/julia-jit", "julia:1")
            .exec("julia -e 'println(\"JL=\", sum(1:1000))'")
            .has("JL=500500").long(),
        // .NET CoreCLR RyuJIT: `dotnet run` compiles IL → native; build forks the SDK toolchain.
        scen("weird/dotnet-ryujit", "mcr.microsoft.com/dotnet/sdk:8.0")
            .exec("mkdir -p /app && cd /app && dotnet new console -o . >/dev/null 2>&1 && cat > Program.cs <<'EOF'\nlong s=0; for(long i=1;i<=1000;i++) s+=i; System.Console.WriteLine(\"NET=\"+s);\nEOF\ndotnet run -c Release 2>/dev/null")
            .has("NET=500500").long().timeout(300)
            .xfail(&Target::LINUX),   // dotnet build/restore fork-exec under dd — GAPS toolchain

        // ===================== unusual languages (interpreters / compilers) ====================
        // Haskell GHC: compile to native then run — ghc forks the assembler/linker (toolchain gap).
        scen("weird/haskell-ghc", "haskell:9.8")
            .exec("echo 'main = putStrLn (\"HS=\" ++ show (sum [1..1000::Int]))' > /m.hs && ghc -O2 -o /mh /m.hs >/dev/null 2>&1 && /mh")
            .has("HS=500500").long()
            .xfail(&Target::LINUX),   // ghc forks as/ld — GAPS toolchain fork-exec
        // R: vectorized numeric reduction through the R interpreter.
        scen("weird/r-compute", "r-base")
            .exec("Rscript -e 'cat(\"R=\", sum(as.numeric(1:1000)), \"\\n\", sep=\"\")'")
            .has("R=500500").long(),
        // Forth: gforth (threaded-code interpreter) sums 1..1000 via a defined word.
        scen("weird/gforth", "debian:bookworm")
            .exec("apt-get update >/dev/null 2>&1 && apt-get install -y gforth >/dev/null 2>&1; gforth -e ': sm 0 1001 1 do i + loop . ; sm cr bye'")
            .has("500500").long(),
        // Tcl: classic string/loop interpreter.
        scen("weird/tcl", "debian:bookworm")
            .exec("apt-get update >/dev/null 2>&1 && apt-get install -y tcl >/dev/null 2>&1; echo 'set s 0; for {set i 1} {$i<=1000} {incr i} {incr s $i}; puts TCL=$s' | tclsh")
            .has("TCL=500500").long(),
        // Lua (PUC-Rio reference interpreter, distinct from LuaJIT).
        scen("weird/lua", "debian:bookworm")
            .exec("apt-get update >/dev/null 2>&1 && apt-get install -y lua5.4 >/dev/null 2>&1; lua5.4 -e 's=0 for i=1,1000 do s=s+i end print(\"LUA=\"..s)'")
            .has("LUA=500500").long(),
        // Perl: heavy regex backtracking over a 300KB string (100k matches).
        scen("weird/perl-regex", "perl:5.40")
            .exec("perl -e '$s=\"abc\"x100000; $n=()=$s=~/abc/g; print \"REGEX=$n\\n\"'")
            .has("REGEX=100000"),
        // BusyBox awk: bytecode-VM text processor.
        scen("weird/awk-compute", "alpine:latest")
            .exec("awk 'BEGIN{for(i=1;i<=1000;i++)s+=i;print \"AWK=\"s}'")
            .has("AWK=500500"),

        // ======================= Python C-extensions (libcrypto / zlib / FFI) ==================
        // zlib deflate/inflate roundtrip (C extension, zlib asm).
        scen("weird/python-zlib", "python:3.12-slim")
            .exec("python3 -c \"import zlib;d=bytes(range(256))*100;print('ZLIB='+str(len(zlib.decompress(zlib.compress(d,9)))))\"")
            .has("ZLIB=25600"),
        // hashlib → OpenSSL libcrypto SHA-256 (SHA-NI / armv8 crypto-ext path).
        scen("weird/python-hashlib", "python:3.12-slim")
            .exec("python3 -c \"import hashlib;print('SHA='+hashlib.sha256(b'abc').hexdigest())\"")
            .has("SHA=ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        // ctypes FFI: dlopen(NULL) + call libc strlen — the foreign-call / trampoline path.
        scen("weird/python-ctypes", "python:3.12-slim")
            .exec("python3 -c \"import ctypes;libc=ctypes.CDLL(None);print('CTYPES='+str(libc.strlen(b'hello')))\"")
            .has("CTYPES=5"),
    ]
}
