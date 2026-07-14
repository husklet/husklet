//! Go — full compile+link+run inside the container (`go run`), heaviest fork/exec+codegen path among
//! the interpreted langs. alpine (musl, internal linker) + bookworm (glibc). Markers per MANIFEST §2.
//! exec-loader-noent triage: NOT an engine gap. go compiles, links (internal linker) and runs
//! correctly on BOTH linux arches under hl — the earlier failures were the pre-seeded poc image dropping
//! Config.Env (PATH=/usr/local/go/bin:/go/bin), so `go` was simply not on PATH. Verified XPASS both arches.

use crate::scenario::{scen, Scenario};

pub fn scenarios() -> Vec<Scenario> {
    vec![
        scen("languages/go-sum-122-alpine", "golang:1.22-alpine")
            .exec("cat > /m.go <<'EOF'\npackage main\nimport \"fmt\"\nfunc main(){ s:=0; for i:=1;i<=1000;i++{ s+=i }; fmt.Println(s) }\nEOF\ngo run /m.go")
            .has("500500")
            .long(),
        scen("languages/go-fib-123-alpine", "golang:1.23-alpine")
            .exec("cat > /m.go <<'EOF'\npackage main\nimport \"fmt\"\nfunc main(){ var a,b uint64 =0,1; for i:=0;i<50;i++{ a,b=b,a+b }; fmt.Println(a) }\nEOF\ngo run /m.go")
            .has("12586269025")
            .long(),
        scen("languages/go-sum-122-bookworm", "golang:1.22-bookworm")
            .exec("cat > /m.go <<'EOF'\npackage main\nimport \"fmt\"\nfunc main(){ s:=0; for i:=1;i<=1000;i++{ s+=i }; fmt.Println(s) }\nEOF\ngo run /m.go")
            .has("500500")
            .long(),
        scen("languages/go-version-122-alpine", "golang:1.22-alpine")
            .exec("go version | grep -o 'go1.22'")
            .has("go1.22"),
    ]
}
