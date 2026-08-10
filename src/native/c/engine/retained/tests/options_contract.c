#include "core/options.h"

#include <stdlib.h>
#include <string.h>

int main(void) {
    hl_options options;
    const char *names[] = {"HL_OVERLAY_UPPER", "HL_GID", "HL_GUEST_ENV_EXACT"};
    const char *values[] = {"", "18446744073709551615", "1"};
    const char *duplicate_names[] = {"HL_GID", "HL_GID"};
    const char *duplicate_values[] = {"1", "2"};
    int status = EXIT_FAILURE;

    if (hl_options_init_records(&options, 3, names, values) != 0) return EXIT_FAILURE;
    if (hl_options_validate(&options) != 0) goto done;
    if (hl_options_get(&options, "HL_CHECKPOINT") != NULL) goto done;
    if (strcmp(hl_options_get(&options, "HL_OVERLAY_UPPER"), "") != 0) goto done;
    if (strcmp(hl_options_get(&options, "HL_GID"), "18446744073709551615") != 0) goto done;
    if (strcmp(hl_options_get(&options, "HL_GUEST_ENV_EXACT"), "1") != 0) goto done;
    if (hl_options_get(&options, "HL_LOG") != NULL) goto done;
    {
        hl_options *previous = hl_options_bind_process(&options);
        hl_options state;
        hl_options *previous_state;
        if (hl_options_set(&options, "HL_GUEST_ENV", "LAUNCH=1", 1) != 0 || hl_options_init(&state) != 0) {
            (void)hl_options_bind_process(previous);
            goto done;
        }
        previous_state = hl_options_bind_process_state(&state);
        if (strcmp(hl_option_get("HL_GUEST_ENV"), "LAUNCH=1") != 0 ||
            hl_process_guest_environment_set("EXEC=1") != 0 ||
            strcmp(hl_process_guest_environment_get(), "EXEC=1") != 0 ||
            strcmp(hl_options_get(&options, "HL_GUEST_ENV"), "LAUNCH=1") != 0 ||
            hl_process_guest_environment_unset() != 0 ||
            strcmp(hl_process_guest_environment_get(), "LAUNCH=1") != 0) {
            (void)hl_options_bind_process_state(previous_state);
            hl_options_destroy(&state);
            (void)hl_options_bind_process(previous);
            goto done;
        }
        (void)hl_options_bind_process_state(previous_state);
        hl_options_destroy(&state);
        (void)hl_options_bind_process(previous);
    }
    options.store_size++;
    if (hl_options_validate(&options) == 0) goto done;
    options.store_size--;
    hl_options_destroy(&options);
    if (hl_options_init_records(&options, 2, duplicate_names, duplicate_values) == 0) {
        hl_options_destroy(&options);
        return EXIT_FAILURE;
    }
    status = EXIT_SUCCESS;

done:
    hl_options_destroy(&options);
    return status;
}
