# Prolonged engine soak suite

These 18 stress workloads remain separate from compatibility tests.
`manifest.tsv` records their deterministic verdicts. The normal lane runs each
case once; the extended lane repeats each case ten times. Both allow four
minutes per case because the `mremap`-heavy realloc workload can take about two
minutes under translation.
