# Time compatibility corpus

These six byte-preserved guests come from legacy `ext_timex`. The manifest retains both production
ISAs, Linux build flags, exact exit status, and deterministic verdict goldens. Timing assertions keep
their original lower bounds and generous deadlines; repeated ABI4 runs exercise delayed delivery and
timer accumulation without replacing them with a native oracle.
