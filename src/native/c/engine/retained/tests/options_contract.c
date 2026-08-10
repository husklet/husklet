#include "core/options.h"

#include <stdlib.h>
#include <string.h>

int main(void) {
    hl_options options;
    int status = EXIT_FAILURE;

    if (hl_options_init(&options) != 0) return EXIT_FAILURE;
    if (hl_options_set(&options, "HL_OVERLAY_UPPER", "/var/lib/husklet/upper", 1) != 0) goto done;
    if (strcmp(hl_options_get(&options, "HL_OVERLAY_UPPER"), "/var/lib/husklet/upper") != 0) goto done;
    if (hl_options_set(&options, "HL_NOT_REGISTERED", "value", 1) != -1) goto done;
    status = EXIT_SUCCESS;

done:
    hl_options_destroy(&options);
    return status;
}
