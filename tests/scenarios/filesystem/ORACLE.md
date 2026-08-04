# Filesystem scenario oracle

These end-to-end cases preserve the images, commands, timeouts, and output
contracts formerly held in the shared filesystem fixture. The manifest and
expected output are owned entirely by this category.

This migration only changes test ownership and representation. It changes no
runtime implementation, so the retained C engine was not used as an
implementation oracle and `/Users/x/dd/engine` was not modified.
