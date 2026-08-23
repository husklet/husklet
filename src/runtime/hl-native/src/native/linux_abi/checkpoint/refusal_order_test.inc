// Proves the refusal result is visible before the best-effort broker notification is attempted.
HL_API int HL_TARGET_LOCAL(checkpoint_refusal_order_test)(void) {
    hl_engine_child_result result;
    int status;
    hl_engine_child_result_begin_for_test(&result);
    g_ckpt_refusal_test_result = &result;
    g_ckpt_refusal_test_observed = 0;
    ckpt_coordinator_report_refusal(CKPT_REFUSAL_SELF_DUMP, "test refusal");
    status = g_ckpt_refusal_test_observed && result.magic == HL_ENGINE_CHILD_RESULT_MAGIC &&
                     result.engine_status == HL_STATUS_NOT_SUPPORTED && result.detail == CKPT_REFUSAL_SELF_DUMP
                 ? 0
                 : -1;
    g_ckpt_refusal_test_result = NULL;
    hl_engine_child_result_end_for_test();
    return status;
}
