#include <stdlib.h>

const char *hl_lint_getenv_fixture(void) {
    return getenv("HL_LINT_TEST");
}
