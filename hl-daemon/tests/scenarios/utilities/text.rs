//! text processing — sed / awk / grep / sort / coreutils pipelines.

use crate::scenario::{scen, Scenario};

pub(super) fn items() -> Vec<Scenario> {
    vec![
        // ---- text processing: sed / awk / grep / sort / coreutils --------------------------------
        scen("utilities/sort-numeric", "alpine")
            .exec("printf '3\\n1\\n2\\n' | sort -n | paste -sd, -")
            .has("1,2,3"),
        // S4 sort|uniq -c counts: a×2,b×2,c×1.
        scen("utilities/sort-uniq-count", "alpine")
            .exec("printf 'b\\na\\nc\\na\\nb\\n' | sort | uniq -c | awk '{print $1$2}' | paste -sd, -")
            .has("2a,2b,1c"),
        scen("utilities/sort-uniq-count-glibc", "debian:bookworm")
            .exec("printf 'b\\na\\nc\\na\\nb\\n' | sort | uniq -c | awk '{print $1$2}' | paste -sd, -")
            .has("2a,2b,1c"),
        // word-frequency pipeline: sort|uniq -c|sort -rn|head|awk — 5-stage fork-heavy pipe.
        scen("utilities/word-frequency", "alpine")
            .exec("printf 'apple\\nbanana\\napple\\ncherry\\nbanana\\napple\\n' | sort | uniq -c | sort -rn | head -1 | awk '{print $2}'")
            .has("apple"),
        scen("utilities/tr-upper", "alpine")
            .exec("echo 'the quick brown' | tr a-z A-Z")
            .has("THE QUICK BROWN"),
        scen("utilities/cut-field", "alpine")
            .exec("echo a:b:c | cut -d: -f2")
            .has("b"),
        scen("utilities/grep-count", "alpine")
            .exec("printf 'foo\\nbar\\nfoo\\n' | grep -c foo")
            .has("2"),
        scen("utilities/sed-substitute", "alpine")
            .exec("echo abcabc | sed 's/a/X/g'")
            .has("XbcXbc"),
        // awk integer compute: sum of squares 1..100 = 338350.
        scen("utilities/awk-squares", "alpine")
            .exec("awk 'BEGIN{for(i=1;i<=100;i++)s+=i*i;print s}'")
            .has("338350"),
        scen("utilities/awk-squares-glibc", "debian:bookworm")
            .exec("awk 'BEGIN{for(i=1;i<=100;i++)s+=i*i;print s}'")
            .has("338350"),
        scen("utilities/head-tail", "alpine")
            .exec("seq 1 10 | head -3 | tail -1")
            .has("3"),
        scen("utilities/wc-lines", "alpine")
            .exec("seq 1 100 | wc -l")
            .has("100"),
        scen("utilities/wc-chars", "alpine")
            .exec("printf abc | wc -c")
            .has("3"),
        // factor: largest prime factor of 500500 = 13 (500500 = 2^2·5^3·7·11·13).
        scen("utilities/factor", "alpine")
            .exec("factor 500500 | tr ' ' '\\n' | tail -n1")
            .has("13"),
        scen("utilities/factor-glibc", "debian:bookworm")
            .exec("factor 500500 | tr ' ' '\\n' | tail -n1")
            .has("13"),
    ]
}
