use super::{Error, diagnostic::Excerpt as _};
use std::collections::BTreeMap;
use std::io::Write;

const BACKEND_TREE_PREFIX: &str = "[diag] backend-tree ";
const BACKEND_SHAPE_PREFIX: &str = "[diag] backend-shape ";
const BACKEND_SHAPE_DETAIL_PREFIX: &str = "[diag] backend-shape-detail ";
const AARCH64_OPCODE_PREFIX: &str = "[diag] aarch64-opcode ";
const AARCH64_OPCODE_FIELDS: &[&str] = &[
    "version",
    "available",
    "body_retired",
    "major0",
    "major1",
    "major2",
    "major3",
    "major4",
    "major5",
    "major6",
    "major7",
    "major8",
    "major9",
    "major10",
    "major11",
    "major12",
    "major13",
    "major14",
    "major15",
    "reserved",
    "load_store",
    "dp_register",
    "dp_immediate",
    "branch_system",
    "simd_fp",
];
const X86_EXIT_FAMILY_PREFIX: &str = "[diag] x86-exit-family ";
const X86_EXIT_FAMILY_FIELDS: &[&str] = &[
    "version",
    "translated_entries",
    "total",
    "t_fallthrough",
    "t_jcc_taken",
    "t_jcc_fall",
    "t_direct_jmp",
    "t_direct_call",
    "t_ret",
    "t_jmp_reg",
    "t_jmp_mem",
    "t_call_reg",
    "t_call_mem",
    "t_syscall",
    "t_irq",
    "t_fault",
    "t_other",
];

pub(crate) fn aarch64_opcode_product(
    stderr: &[u8],
    required: bool,
    require_nonzero: bool,
    reconcile_shape: bool,
) -> Result<Option<BTreeMap<&str, u64>>, Error> {
    let stderr = std::str::from_utf8(stderr).map_err(|_| "aarch64-opcode diagnostic is not UTF-8")?;
    let records: Vec<_> = stderr
        .lines()
        .filter_map(|line| line.strip_prefix(AARCH64_OPCODE_PREFIX))
        .collect();
    if !required {
        if records.is_empty() {
            return Ok(None);
        }
        return Err("aarch64-opcode diagnostic appeared while disabled".into());
    }
    if records.len() != 1 {
        return Err(format!(
            "aarch64-opcode diagnostic appeared {} times, expected once",
            records.len()
        )
        .into());
    }
    let mut values = BTreeMap::new();
    let mut order = Vec::new();
    for field in records[0].split_ascii_whitespace() {
        let (name, value) = field
            .split_once('=')
            .ok_or_else(|| format!("aarch64-opcode malformed field {field:?}"))?;
        if !AARCH64_OPCODE_FIELDS.contains(&name) {
            return Err(format!("aarch64-opcode unknown field {name:?}").into());
        }
        if values
            .insert(
                name,
                value
                    .parse::<u64>()
                    .map_err(|_| format!("aarch64-opcode field {name:?} is not an integer"))?,
            )
            .is_some()
        {
            return Err(format!("aarch64-opcode duplicate field {name:?}").into());
        }
        order.push(name);
    }
    if order != AARCH64_OPCODE_FIELDS {
        return Err("aarch64-opcode fields are omitted or out of order".into());
    }
    if values["version"] != 1 || values["available"] != 1 {
        return Err("aarch64-opcode version/availability is invalid".into());
    }
    let majors = (0..16).try_fold(0u64, |sum, i| {
        sum.checked_add(values[format!("major{i}").as_str()])
            .ok_or("aarch64-opcode major sum overflow")
    })?;
    let families = [
        "reserved",
        "load_store",
        "dp_register",
        "dp_immediate",
        "branch_system",
        "simd_fp",
    ]
    .into_iter()
    .try_fold(0u64, |sum, name| {
        sum.checked_add(values[name])
            .ok_or("aarch64-opcode family sum overflow")
    })?;
    if majors != values["body_retired"] || families != values["body_retired"] {
        return Err("aarch64-opcode counters do not reconcile".into());
    }
    if require_nonzero && values["body_retired"] == 0 {
        return Err("aarch64-opcode dedicated fixture retired no instructions".into());
    }
    if reconcile_shape {
        let tree = backend_tree(stderr)?.ok_or("aarch64-opcode product omitted backend-tree")?;
        if tree["interpreted_steps"] != values["body_retired"] {
            return Err("aarch64-opcode retired total differs from aggregated interpreted steps".into());
        }
    }
    Ok(Some(values))
}
const BACKEND_SHAPE_PRODUCT_FIELDS: &[&str] = &[
    "version",
    "available",
    "mixed_sse_executed",
    "mixed_sse_executed_transitions",
    "mixed_sse_disabled_boundaries",
    "jcc_ibtc_enabled",
    "jcc_ibtc_emitted",
    "jcc_ibtc_hits",
    "jcc_ibtc_misses",
    "jcc_ibtc_irq",
    "jcc_ibtc_fills",
    "jcc_ibtc_suppressed",
    "jcc_ibtc_invalid_refusals",
    "direct_jmp_ibtc_enabled",
    "direct_jmp_ibtc_emitted",
    "direct_jmp_ibtc_hits",
    "direct_jmp_ibtc_misses",
    "direct_jmp_ibtc_irq",
    "direct_jmp_ibtc_fills",
    "direct_jmp_ibtc_suppressed",
    "direct_jmp_ibtc_invalid_refusals",
];
const BACKEND_SHAPE_PRODUCT_V5_EXTRA: &[&str] = &[
    "crossings",
    "translated_entries",
    "interpreted_entries",
    "translated_steps",
    "interpreted_steps",
    "direct_call_ibtc_emitted",
    "direct_call_ibtc_hits",
    "direct_call_ibtc_misses",
    "direct_call_ibtc_irq",
    "direct_call_ibtc_fills",
    "direct_call_ibtc_invalid_refusals",
    "ret_ibtc_attempts",
    "ret_ibtc_hits",
    "ret_ibtc_key_misses",
    "ret_ibtc_null_misses",
    "ret_ibtc_irq",
    "ret_ibtc_fills",
    "ret_ibtc_collisions",
    "ret_ibtc_unmapped",
    "ret_ibtc_invalid_refusals",
    "ret_fast_ibtc_hits",
    "ret_fast_ibtc_misses",
    "ret_fast_ibtc_irq",
    "ret_fast_ibtc_fills",
    "ret_fast_ibtc_invalid_refusals",
];
const BACKEND_SHAPE_PRODUCT_V6_EXTRA: &[&str] =
    &["executed_form_total", "executed_form_unique", "executed_form_overflow"];
const BACKEND_SHAPE_PRODUCT_V9_EXTRA: &[&str] = &[
    "jcc_taken_ibtc_misses",
    "indirect_ibtc_misses",
    "jcc_late_candidate",
    "jcc_late_eligible",
    "jcc_late_invalid",
    "jcc_late_target_absent",
    "jcc_late_page_generation",
    "jcc_late_displacement",
    "jcc_late_other",
    "jcc_invalid_null",
    "jcc_invalid_magic",
    "jcc_invalid_gpc",
    "jcc_invalid_block_generation",
    "jcc_invalid_entry_zero",
    "jcc_invalid_length_zero",
    "jcc_invalid_resolve",
    "jcc_invalid_resolved_generation",
    "jcc_invalid_entry_overflow",
    "jcc_invalid_site_unique",
    "jcc_invalid_site_overflow",
];
const BACKEND_SHAPE_PRODUCT_V10_EXTRA: &[&str] = &[
    "lifecycle_settled",
    "missing_claims",
    "duplicate_finalize",
    "reserved",
    "live",
    "claimed",
];
const BACKEND_SHAPE_PRODUCT_V11_EXTRA: &[&str] = &[
    "first_finalize_caller",
    "first_finalize_actor",
    "first_finalize_slot_pid",
    "duplicate_finalize_caller",
    "duplicate_finalize_actor",
    "duplicate_finalize_slot_pid",
];
const BACKEND_SHAPE_PRODUCT_V12_EXTRA: &[&str] = &["duplicate_slot_first_caller", "duplicate_slot_first_actor"];
const BACKEND_SHAPE_PRODUCT_V13_EXTRA: &[&str] = &[
    "translation_codegen_available",
    "jcc_ibtc_fill_empty",
    "jcc_ibtc_fill_collision",
    "jcc_ibtc_fill_irq_cause",
    "jcc_ibtc_fill_same_key",
    "jcc_late_site_first",
    "jcc_late_site_repeated",
    "jcc_late_site_stable",
    "jcc_late_site_changed",
    "jcc_late_site_current",
    "jcc_late_site_retired",
    "jcc_late_site_unique",
    "jcc_late_site_overflow",
    "jcc_late_site_abandoned",
    "jcc_late_site_max",
];

// Protocol v13 is the current production contract. Keep its wire order explicit: accepting the same
// names in a different order would let a positional producer/argument mismatch survive this consumer.
const BACKEND_SHAPE_PRODUCT_V13_ORDER: &[&str] = &[
    "version",
    "available",
    "translation_codegen_available",
    "lifecycle_settled",
    "missing_claims",
    "duplicate_finalize",
    "reserved",
    "live",
    "claimed",
    "first_finalize_caller",
    "first_finalize_actor",
    "first_finalize_slot_pid",
    "duplicate_finalize_caller",
    "duplicate_finalize_actor",
    "duplicate_finalize_slot_pid",
    "duplicate_slot_first_caller",
    "duplicate_slot_first_actor",
    "crossings",
    "translated_entries",
    "interpreted_entries",
    "translated_steps",
    "interpreted_steps",
    "mixed_sse_executed",
    "mixed_sse_executed_transitions",
    "mixed_sse_disabled_boundaries",
    "jcc_ibtc_enabled",
    "jcc_ibtc_emitted",
    "jcc_ibtc_hits",
    "jcc_ibtc_misses",
    "jcc_ibtc_irq",
    "jcc_ibtc_fills",
    "jcc_ibtc_suppressed",
    "jcc_ibtc_invalid_refusals",
    "jcc_ibtc_fill_empty",
    "jcc_ibtc_fill_collision",
    "jcc_ibtc_fill_irq_cause",
    "jcc_ibtc_fill_same_key",
    "jcc_taken_ibtc_misses",
    "indirect_ibtc_misses",
    "jcc_late_candidate",
    "jcc_late_eligible",
    "jcc_late_invalid",
    "jcc_late_target_absent",
    "jcc_late_page_generation",
    "jcc_late_displacement",
    "jcc_late_other",
    "jcc_late_site_first",
    "jcc_late_site_repeated",
    "jcc_late_site_stable",
    "jcc_late_site_changed",
    "jcc_late_site_current",
    "jcc_late_site_retired",
    "jcc_late_site_unique",
    "jcc_late_site_overflow",
    "jcc_late_site_abandoned",
    "jcc_late_site_max",
    "jcc_invalid_null",
    "jcc_invalid_magic",
    "jcc_invalid_gpc",
    "jcc_invalid_block_generation",
    "jcc_invalid_entry_zero",
    "jcc_invalid_length_zero",
    "jcc_invalid_resolve",
    "jcc_invalid_resolved_generation",
    "jcc_invalid_entry_overflow",
    "jcc_invalid_site_unique",
    "jcc_invalid_site_overflow",
    "direct_jmp_ibtc_enabled",
    "direct_jmp_ibtc_emitted",
    "direct_jmp_ibtc_hits",
    "direct_jmp_ibtc_misses",
    "direct_jmp_ibtc_irq",
    "direct_jmp_ibtc_fills",
    "direct_jmp_ibtc_suppressed",
    "direct_jmp_ibtc_invalid_refusals",
    "direct_call_ibtc_emitted",
    "direct_call_ibtc_hits",
    "direct_call_ibtc_misses",
    "direct_call_ibtc_irq",
    "direct_call_ibtc_fills",
    "direct_call_ibtc_invalid_refusals",
    "ret_ibtc_attempts",
    "ret_ibtc_hits",
    "ret_ibtc_key_misses",
    "ret_ibtc_null_misses",
    "ret_ibtc_irq",
    "ret_ibtc_fills",
    "ret_ibtc_collisions",
    "ret_ibtc_unmapped",
    "ret_ibtc_invalid_refusals",
    "ret_fast_ibtc_hits",
    "ret_fast_ibtc_misses",
    "ret_fast_ibtc_irq",
    "ret_fast_ibtc_fills",
    "ret_fast_ibtc_invalid_refusals",
    "executed_form_total",
    "executed_form_unique",
    "executed_form_overflow",
];

fn backend_shape_product_field(name: &str, version: u64) -> bool {
    if BACKEND_SHAPE_PRODUCT_FIELDS.contains(&name) {
        return true;
    }
    if version < 5 {
        return false;
    }
    if BACKEND_SHAPE_PRODUCT_V5_EXTRA.contains(&name) {
        return true;
    }
    if version < 6 {
        return false;
    }
    if BACKEND_SHAPE_PRODUCT_V6_EXTRA.contains(&name) {
        return true;
    }
    if version >= 9 && BACKEND_SHAPE_PRODUCT_V9_EXTRA.contains(&name) {
        return true;
    }
    if version >= 10 && BACKEND_SHAPE_PRODUCT_V10_EXTRA.contains(&name) {
        return true;
    }
    if version >= 11 && BACKEND_SHAPE_PRODUCT_V11_EXTRA.contains(&name) {
        return true;
    }
    if version >= 12 && BACKEND_SHAPE_PRODUCT_V12_EXTRA.contains(&name) {
        return true;
    }
    if version >= 13 && BACKEND_SHAPE_PRODUCT_V13_EXTRA.contains(&name) {
        return true;
    }
    let Some(suffix) = name.strip_prefix("executed_form") else {
        return false;
    };
    let Some((rank, kind)) = suffix.split_once('_') else {
        return false;
    };
    rank.parse::<u8>().is_ok_and(|rank| rank < 16) && matches!(kind, "key" | "count")
}

fn backend_shape_product_v13_order(fields: &[&str]) -> bool {
    if fields.len() != BACKEND_SHAPE_PRODUCT_V13_ORDER.len() + 32
        || fields[..BACKEND_SHAPE_PRODUCT_V13_ORDER.len()] != *BACKEND_SHAPE_PRODUCT_V13_ORDER
    {
        return false;
    }
    fields[BACKEND_SHAPE_PRODUCT_V13_ORDER.len()..]
        .chunks_exact(2)
        .enumerate()
        .all(|(rank, pair)| {
            pair[0] == format!("executed_form{rank}_key") && pair[1] == format!("executed_form{rank}_count")
        })
}
const BACKEND_TREE_FIELDS: [&str; 33] = [
    "version",
    "root_pid",
    "claimed",
    "completed",
    "abnormal",
    "missing",
    "duplicate_finalize",
    "crossings",
    "translated_entries",
    "interpreted_entries",
    "translated_steps",
    "interpreted_steps",
    "translations",
    "map_hits",
    "stw_retries",
    "irq_pending",
    "reason0",
    "reason1",
    "reason2",
    "reason3",
    "reason4",
    "reason5",
    "reason6",
    "reason7",
    "reason8",
    "reason9",
    "reason10",
    "reason11",
    "reason12",
    "reason13",
    "reason14",
    "reason15",
    "reason_other",
];

const BACKEND_SHAPE_FIELDS: &[&str] = &[
    "version",
    "translated_entries",
    "translated_transfers",
    "t_fallthrough",
    "t_cond_taken",
    "t_cond_not_taken",
    "t_direct_jump",
    "t_direct_call",
    "t_return",
    "t_indirect_branch",
    "t_indirect_call",
    "t_syscall",
    "t_irq",
    "t_fault",
    "t_other",
    "fall_total",
    "fall_cap",
    "fall_decode",
    "fall_normal_to_sse2",
    "fall_sse2_to_normal",
    "fall_normal_to_fs",
    "fall_fs_to_normal",
    "fall_sse2_to_fs",
    "fall_fs_to_sse2",
    "fall_tl_no",
    "fall_displaced",
    "fall_fetch",
    "fall_riprel",
    "fall_fs_transaction",
    "fall_sse_riprel",
    "fall_other",
    "stitch_jmp",
    "stitch_cond_fall",
    "e_fall_total",
    "e_fall_mapped",
    "e_fall_unmapped",
    "e_fall_interrupted",
    "e_fall_chained",
    "e_fall_dispatcher",
    "e_jt_total",
    "e_jt_mapped",
    "e_jt_unmapped",
    "e_jt_interrupted",
    "e_jt_chained",
    "e_jt_dispatcher",
    "e_jn_total",
    "e_jn_mapped",
    "e_jn_unmapped",
    "e_jn_interrupted",
    "e_jn_chained",
    "e_jn_dispatcher",
    "e_jmp_total",
    "e_jmp_mapped",
    "e_jmp_unmapped",
    "e_jmp_interrupted",
    "e_jmp_chained",
    "e_jmp_dispatcher",
    "e_call_total",
    "e_call_mapped",
    "e_call_unmapped",
    "e_call_interrupted",
    "e_call_chained",
    "e_call_dispatcher",
    "jt_same_page",
    "jt_cross_page",
    "jt_target_translated",
    "jt_target_interpreted",
    "jt_generation_current",
    "jt_generation_retired",
    "jt_rel32",
    "jt_rel32_unreachable",
    "jt_eligible",
    "jt_ineligible",
    "interpreted_entries",
    "i_disabled",
    "i_image",
    "i_decode",
    "i_unsupported",
    "i_authority",
    "i_resource",
    "i_emit",
    "i_runtime_image",
    "i_runtime_bind",
    "i_other",
    "s_fallthrough",
    "s_cond_taken",
    "s_cond_not_taken",
    "s_direct_jump",
    "s_direct_call",
    "s_return",
    "s_indirect_branch",
    "s_indirect_call",
    "s_syscall",
    "s_irq",
    "s_fault",
    "s_service",
    "s_other",
    "fallback_total",
    "fallback_unique",
    "fallback_overflow",
    "stop_total",
    "stop_unique",
    "stop_overflow",
    "family_jmem",
    "family_div_total",
    "family_div_inline",
    "family_div_service64",
    "family_div_service64_completed",
    "family_div_de",
    "family_idiv_total",
    "family_idiv_inline",
    "family_idiv_service64",
    "family_idiv_service64_completed",
    "family_idiv_de",
    "family_total",
    "mixed_sse_executed",
    "mixed_sse_executed_transitions",
    "mixed_sse_disabled_boundaries",
    "fallback0_key",
    "fallback0_count",
    "fallback1_key",
    "fallback1_count",
    "fallback2_key",
    "fallback2_count",
    "fallback3_key",
    "fallback3_count",
    "fallback4_key",
    "fallback4_count",
    "fallback5_key",
    "fallback5_count",
    "fallback6_key",
    "fallback6_count",
    "fallback7_key",
    "fallback7_count",
    "stop0_key",
    "stop0_count",
    "stop1_key",
    "stop1_count",
    "stop2_key",
    "stop2_count",
    "stop3_key",
    "stop3_count",
    "stop4_key",
    "stop4_count",
    "stop5_key",
    "stop5_count",
    "stop6_key",
    "stop6_count",
    "stop7_key",
    "stop7_count",
    "direct_call_ibtc_emitted",
    "direct_call_ibtc_hits",
    "direct_call_ibtc_misses",
    "direct_call_ibtc_irq",
    "direct_call_ibtc_fills",
    "direct_call_ibtc_invalid_refusals",
    "direct_call_ibtc_fast_redispatch",
    "direct_call_guard_candidate_enabled",
    "direct_call_guard_attempts",
    "direct_call_guard_fast_hits",
    "direct_call_guard_key_misses",
    "direct_call_guard_null_misses",
    "direct_call_guard_irq",
    "direct_call_guard_slow_entries",
];

pub(super) fn validate_profile(stderr: &str) -> Result<(), Error> {
    let mut crossings = None;
    let mut translations = None;
    for field in stderr
        .lines()
        .filter_map(|line| line.strip_prefix("[prof] "))
        .flat_map(str::split_whitespace)
    {
        let Some((name, value)) = field.split_once('=') else {
            continue;
        };
        let destination = match name {
            "crossings" => &mut crossings,
            "translations" => &mut translations,
            _ => continue,
        };
        *destination = Some(
            value
                .parse::<u64>()
                .map_err(|_| format!("retained C {name} is not an integer"))?,
        );
    }
    if crossings.is_none() || translations.is_none() {
        return Err("retained C profile omitted the crossings/translations summary".into());
    }
    Ok(())
}

pub(super) fn validate_profile_or_product(stderr: &[u8]) -> Result<(), Error> {
    let text = std::str::from_utf8(stderr)?;
    if product_backend_shape(stderr) {
        let shape = backend_shape_product(stderr, true)?.expect("product shape cardinality");
        let coherent = shape
            .get("crossings")
            .zip(shape.get("translated_entries"))
            .zip(shape.get("interpreted_entries"))
            .is_some_and(|((crossings, translated), interpreted)| {
                translated.checked_add(*interpreted) == Some(*crossings)
            });
        if coherent
            && shape.get("translated_entries").is_some_and(|value| *value > 0)
            && shape.get("translated_steps").is_some_and(|value| *value > 0)
        {
            return Ok(());
        }
    } else if let Some(tree) = backend_tree(text)? {
        if tree["crossings"] > 0
            && tree["translations"] > 0
            && tree["translated_entries"] > 0
            && tree["translated_steps"] > 0
        {
            return Ok(());
        }
    }
    validate_profile(text)
}

pub(super) fn validate_backend_tree(stderr: &[u8], enabled: bool) -> Result<(), Error> {
    let product = product_backend_shape(stderr);
    let records = stderr
        .split(|byte| *byte == b'\n')
        .filter(|line| line.starts_with(BACKEND_TREE_PREFIX.as_bytes()))
        .count();
    if product {
        if records != 0 {
            return Err("backend-shape product diagnostic cannot accompany backend-tree".into());
        }
        backend_shape_product(stderr, enabled)?;
        return Ok(());
    }
    let expected = usize::from(enabled);
    if records != expected {
        return Err(format!("backend-tree diagnostic appeared {records} times, expected {expected}").into());
    }
    if !enabled {
        let shapes = stderr
            .split(|byte| *byte == b'\n')
            .filter(|line| {
                line.starts_with(BACKEND_SHAPE_DETAIL_PREFIX.as_bytes())
                    || line.starts_with(BACKEND_SHAPE_PREFIX.as_bytes())
            })
            .count();
        if shapes != 0 {
            return Err(format!("backend-shape diagnostic appeared {shapes} times, expected 0").into());
        }
        return Ok(());
    }
    let text = std::str::from_utf8(stderr).map_err(|_| "backend-tree diagnostic stderr is not UTF-8")?;
    let tree = backend_tree(text)?.expect("cardinality check established one backend-tree record");
    let shape = backend_shape(text)?;
    if tree["translated_entries"] != shape["translated_entries"]
        || tree["interpreted_entries"] != shape["interpreted_entries"]
    {
        return Err("backend-shape entries do not match backend-tree".into());
    }
    Ok(())
}

pub(super) fn validate_direct_call_guard_candidate(stderr: &[u8], expected: bool) -> Result<(), Error> {
    let text = std::str::from_utf8(stderr).map_err(|_| "direct-call guard diagnostic is not UTF-8")?;
    let shape = backend_shape(text)?;
    let observed = shape["direct_call_guard_candidate_enabled"];
    if observed != u64::from(expected) {
        return Err(format!(
            "direct-call guard candidate is {observed}, expected {}",
            u64::from(expected)
        )
        .into());
    }
    Ok(())
}

pub(super) fn validate_translated_execution(stderr: &[u8]) -> Result<(), Error> {
    if product_backend_shape(stderr) {
        let shape = backend_shape_product(stderr, true)?.expect("product-shape detection established one record");
        if shape.get("translation_codegen_available") == Some(&0) {
            return Err("translated execution backend-shape reports translation codegen unavailable on this host/guest ISA pairing".into());
        }
        if shape["translated_entries"] == 0 {
            return Err("translated execution backend-shape reported zero translated entries".into());
        }
        return Ok(());
    }
    let text = std::str::from_utf8(stderr).map_err(|_| "translated backend receipt is not UTF-8")?;
    let tree = backend_tree(text)?.ok_or("translated execution emitted no backend-tree receipt")?;
    if tree["translated_entries"] == 0 {
        return Err("translated execution backend-tree reported zero translated entries".into());
    }
    Ok(())
}

fn product_backend_shape(stderr: &[u8]) -> bool {
    std::str::from_utf8(stderr).is_ok_and(|text| {
        text.lines()
            .filter_map(|line| line.strip_prefix(BACKEND_SHAPE_PREFIX))
            .filter_map(|record| {
                record
                    .split_whitespace()
                    .find_map(|field| field.strip_prefix("version=")?.parse::<u64>().ok())
            })
            .any(|version| version >= 4)
    })
}

fn backend_tree(stderr: &str) -> Result<Option<BTreeMap<&str, u64>>, Error> {
    let records = stderr
        .lines()
        .filter_map(|line| line.strip_prefix(BACKEND_TREE_PREFIX))
        .collect::<Vec<_>>();
    if records.is_empty() {
        return Ok(None);
    }
    if records.len() != 1 {
        return Err(format!(
            "backend-tree diagnostic appeared {} times, expected once",
            records.len()
        )
        .into());
    }
    let mut fields = BTreeMap::new();
    for field in records[0].split_whitespace() {
        let Some((name, value)) = field.split_once('=') else {
            return Err(format!("backend-tree diagnostic has malformed field {field:?}").into());
        };
        if !BACKEND_TREE_FIELDS.contains(&name) {
            return Err(format!("backend-tree diagnostic has unknown field {name:?}").into());
        }
        let value = value
            .parse::<u64>()
            .map_err(|_| format!("backend-tree field {name:?} is not an integer"))?;
        if fields.insert(name, value).is_some() {
            return Err(format!("backend-tree diagnostic duplicates field {name:?}").into());
        }
    }
    for name in BACKEND_TREE_FIELDS {
        if !fields.contains_key(name) {
            return Err(format!("backend-tree diagnostic omitted field {name:?}").into());
        }
    }
    if fields["version"] != 1 || fields["root_pid"] == 0 {
        return Err("backend-tree diagnostic has invalid version or root pid".into());
    }
    let lifecycle = fields["completed"]
        .checked_add(fields["abnormal"])
        .and_then(|value| value.checked_add(fields["missing"]));
    if lifecycle != Some(fields["claimed"]) {
        return Err("backend-tree lifecycle totals do not reconcile".into());
    }
    if fields["translated_entries"].checked_add(fields["interpreted_entries"]) != Some(fields["crossings"]) {
        return Err("backend-tree entry totals do not reconcile with crossings".into());
    }
    let reasons = (0..16).try_fold(0_u64, |total, reason| {
        total.checked_add(fields[format!("reason{reason}").as_str()])
    });
    let reasons = reasons.and_then(|total| total.checked_add(fields["reason_other"]));
    if reasons != Some(fields["crossings"]) {
        return Err("backend-tree reason totals do not reconcile with crossings".into());
    }
    Ok(Some(fields))
}

#[derive(Debug, Eq, PartialEq)]
struct JccPathEvidence {
    /// The v4-v13 wire name is `jcc_ibtc_hits`, but this counter executes only in the shared stub;
    /// an inline IBTC hit jumps directly to the cached body and cannot reach it.
    jcc_ibtc_stub_hits: u64,
    /// Existing direct/prelinked JCC evidence, summed across the per-process translator reports.
    /// Absence remains explicit because product-only reporting does not carry this local counter.
    jcc_link_taken: Option<u64>,
}

fn jcc_path_evidence(stderr: &[u8]) -> Result<JccPathEvidence, Error> {
    let product = backend_shape_product(stderr, true)?
        .ok_or("backend-shape product diagnostic is unavailable")?;
    let text = std::str::from_utf8(stderr).map_err(|_| "JCC path evidence is not UTF-8")?;
    let mut jcc_link_taken = None::<u64>;
    for record in text.lines().filter_map(|line| line.strip_prefix("[prof] translit: ")) {
        let mut record_value = None;
        for field in record.split_whitespace() {
            let Some(value) = field.strip_prefix("jcc_link_taken=") else { continue };
            if record_value.is_some() {
                return Err("translit profile duplicates jcc_link_taken".into());
            }
            record_value = Some(value.parse::<u64>()
                .map_err(|_| "translit profile jcc_link_taken is not an integer")?);
        }
        if let Some(value) = record_value {
            jcc_link_taken = Some(jcc_link_taken.unwrap_or(0).checked_add(value)
                .ok_or("translit profile jcc_link_taken total overflow")?);
        }
    }
    Ok(JccPathEvidence {
        jcc_ibtc_stub_hits: product["jcc_ibtc_hits"],
        jcc_link_taken,
    })
}

fn backend_shape(stderr: &str) -> Result<BTreeMap<&str, u64>, Error> {
    let records = stderr
        .lines()
        .filter_map(|line| {
            if let Some(record) = line.strip_prefix(BACKEND_SHAPE_DETAIL_PREFIX) {
                return Some((record, 2));
            }
            /* Legacy unit fixtures predate the wire-prefix split. */
            line.strip_prefix(BACKEND_SHAPE_PREFIX)
                .filter(|record| record.starts_with("version=1 "))
                .map(|record| (record, 1))
        })
        .collect::<Vec<_>>();
    if records.len() != 1 {
        return Err(format!(
            "backend-shape diagnostic appeared {} times, expected once",
            records.len()
        )
        .into());
    }
    let mut fields = BTreeMap::new();
    let (record, expected_version) = records[0];
    for field in record.split_whitespace() {
        let Some((name, value)) = field.split_once('=') else {
            return Err(format!("backend-shape diagnostic has malformed field {field:?}").into());
        };
        if !BACKEND_SHAPE_FIELDS.contains(&name) {
            return Err(format!("backend-shape diagnostic has unknown field {name:?}").into());
        }
        let value = value
            .parse::<u64>()
            .map_err(|_| format!("backend-shape field {name:?} is not an integer"))?;
        if fields.insert(name, value).is_some() {
            return Err(format!("backend-shape diagnostic duplicates field {name:?}").into());
        }
    }
    for name in BACKEND_SHAPE_FIELDS {
        if !fields.contains_key(name) {
            return Err(format!("backend-shape diagnostic omitted field {name:?}").into());
        }
    }
    if fields["version"] != expected_version {
        return Err("backend-shape diagnostic has invalid version".into());
    }
    let sum = |names: &[&str]| {
        names
            .iter()
            .try_fold(0_u64, |total, name| total.checked_add(fields[name]))
    };
    let translated_exits = sum(&[
        "t_fallthrough",
        "t_cond_taken",
        "t_cond_not_taken",
        "t_direct_jump",
        "t_direct_call",
        "t_return",
        "t_indirect_branch",
        "t_indirect_call",
        "t_syscall",
        "t_irq",
        "t_fault",
        "t_other",
    ]);
    if translated_exits != Some(fields["translated_entries"]) {
        return Err("backend-shape translated exits do not reconcile with entries".into());
    }
    let fall_stops = sum(&[
        "fall_cap",
        "fall_decode",
        "fall_normal_to_sse2",
        "fall_sse2_to_normal",
        "fall_normal_to_fs",
        "fall_fs_to_normal",
        "fall_sse2_to_fs",
        "fall_fs_to_sse2",
        "fall_tl_no",
        "fall_displaced",
        "fall_fetch",
        "fall_riprel",
        "fall_fs_transaction",
        "fall_sse_riprel",
        "fall_other",
    ]);
    if fall_stops != Some(fields["fall_total"]) || fall_stops != Some(fields["t_fallthrough"]) {
        return Err("backend-shape fall-stop reasons do not reconcile with translated fallthroughs".into());
    }
    let transfers = sum(&[
        "translated_entries",
        "stitch_jmp",
        "stitch_cond_fall",
        "e_fall_chained",
        "e_jt_chained",
        "e_jn_chained",
        "e_jmp_chained",
        "e_call_chained",
    ]);
    if transfers != Some(fields["translated_transfers"]) {
        return Err("backend-shape translated transfers do not reconcile".into());
    }
    for family in ["fall", "jt", "jn", "jmp", "call"] {
        let total = fields[format!("e_{family}_total").as_str()];
        let resolutions = fields[format!("e_{family}_mapped").as_str()]
            .checked_add(fields[format!("e_{family}_unmapped").as_str()])
            .and_then(|value| value.checked_add(fields[format!("e_{family}_interrupted").as_str()]));
        if resolutions != Some(total) {
            return Err(format!("backend-shape {family} edge map dispositions do not reconcile").into());
        }
        let executions = fields[format!("e_{family}_chained").as_str()]
            .checked_add(fields[format!("e_{family}_dispatcher").as_str()]);
        if executions != Some(total) {
            return Err(format!("backend-shape {family} edge execution dispositions do not reconcile").into());
        }
    }
    if fields["jt_same_page"].checked_add(fields["jt_cross_page"]) != Some(fields["e_jt_total"]) {
        return Err("backend-shape Jcc-taken source-page dispositions do not reconcile".into());
    }
    if fields["jt_target_translated"].checked_add(fields["jt_target_interpreted"]) != Some(fields["e_jt_mapped"]) {
        return Err("backend-shape Jcc-taken mapped-target kinds do not reconcile".into());
    }
    if fields["jt_generation_current"].checked_add(fields["jt_generation_retired"])
        != Some(fields["jt_target_translated"])
    {
        return Err("backend-shape Jcc-taken target generations do not reconcile".into());
    }
    if fields["jt_rel32"].checked_add(fields["jt_rel32_unreachable"]) != Some(fields["jt_target_translated"]) {
        return Err("backend-shape Jcc-taken rel32 dispositions do not reconcile".into());
    }
    let eligibility = fields["jt_eligible"]
        .checked_add(fields["jt_ineligible"])
        .and_then(|value| value.checked_add(fields["e_jt_interrupted"]));
    if eligibility != Some(fields["e_jt_total"])
        || fields["jt_eligible"] > fields["jt_same_page"]
        || fields["jt_eligible"] > fields["jt_target_translated"]
        || fields["jt_eligible"] > fields["jt_generation_current"]
        || fields["jt_eligible"] > fields["jt_rel32"]
    {
        return Err("backend-shape Jcc-taken eligibility does not reconcile".into());
    }
    let interpreter_entries = sum(&[
        "i_disabled",
        "i_image",
        "i_decode",
        "i_unsupported",
        "i_authority",
        "i_resource",
        "i_emit",
        "i_runtime_image",
        "i_runtime_bind",
        "i_other",
    ]);
    if interpreter_entries != Some(fields["interpreted_entries"]) {
        return Err("backend-shape interpreter entry causes do not reconcile".into());
    }
    let interpreter_stops = sum(&[
        "s_fallthrough",
        "s_cond_taken",
        "s_cond_not_taken",
        "s_direct_jump",
        "s_direct_call",
        "s_return",
        "s_indirect_branch",
        "s_indirect_call",
        "s_syscall",
        "s_irq",
        "s_fault",
        "s_service",
        "s_other",
    ]);
    if interpreter_stops != Some(fields["interpreted_entries"]) {
        return Err("backend-shape interpreter stop causes do not reconcile".into());
    }
    if fields["fallback_total"] != fields["i_unsupported"] {
        return Err("backend-shape fallback forms do not reconcile with unsupported entries".into());
    }
    if sum(&["family_div_inline", "family_div_service64", "family_div_de"]) != Some(fields["family_div_total"]) {
        return Err("backend-shape DIV family outcomes do not reconcile".into());
    }
    if sum(&["family_idiv_inline", "family_idiv_service64", "family_idiv_de"]) != Some(fields["family_idiv_total"]) {
        return Err("backend-shape IDIV family outcomes do not reconcile".into());
    }
    if fields["family_div_service64_completed"] > fields["family_div_service64"]
        || fields["family_idiv_service64_completed"] > fields["family_idiv_service64"]
    {
        return Err("backend-shape deferred divide completions exceed requests".into());
    }
    if sum(&["family_jmem", "family_div_total", "family_idiv_total"]) != Some(fields["family_total"]) {
        return Err("backend-shape executed-family totals do not reconcile".into());
    }
    if fields["mixed_sse_executed_transitions"] < fields["mixed_sse_executed"]
        || (fields["mixed_sse_executed"] == 0 && fields["mixed_sse_executed_transitions"] != 0)
    {
        return Err("backend-shape mixed-SSE execution totals do not reconcile".into());
    }
    if fields["mixed_sse_executed"] != 0 && fields["mixed_sse_disabled_boundaries"] != 0 {
        return Err("backend-shape mixed-SSE enabled/disabled execution polarity is inconsistent".into());
    }
    Ok(fields)
}

pub(crate) fn backend_shape_product(stderr: &[u8], enabled: bool) -> Result<Option<BTreeMap<&str, u64>>, Error> {
    let stderr = std::str::from_utf8(stderr).map_err(|_| "backend-shape product diagnostic is not UTF-8")?;
    let records = stderr
        .lines()
        .filter_map(|line| line.strip_prefix(BACKEND_SHAPE_PREFIX))
        .collect::<Vec<_>>();
    if !enabled {
        if records.is_empty() {
            return Ok(None);
        }
        return Err(format!(
            "backend-shape product diagnostic appeared {} times, expected 0",
            records.len()
        )
        .into());
    }
    if records.len() != 1 {
        return Err(format!(
            "backend-shape product diagnostic appeared {} times, expected once",
            records.len()
        )
        .into());
    }
    let version = records[0]
        .split_whitespace()
        .find_map(|field| field.strip_prefix("version=")?.parse::<u64>().ok())
        .unwrap_or(0);
    let mut fields = BTreeMap::new();
    let mut field_order = Vec::new();
    for field in records[0].split_whitespace() {
        let Some((name, value)) = field.split_once('=') else {
            return Err(format!("backend-shape product diagnostic has malformed field {field:?}").into());
        };
        if !backend_shape_product_field(name, version) {
            return Err(format!("backend-shape product diagnostic has unknown field {name:?}").into());
        }
        let value = value
            .parse::<u64>()
            .map_err(|_| format!("backend-shape product field {name:?} is not an integer"))?;
        if fields.insert(name, value).is_some() {
            return Err(format!("backend-shape product diagnostic duplicates field {name:?}").into());
        }
        field_order.push(name);
    }
    for name in BACKEND_SHAPE_PRODUCT_FIELDS {
        if !fields.contains_key(name) {
            return Err(format!("backend-shape product diagnostic omitted field {name:?}").into());
        }
    }
    if version >= 5 {
        for name in BACKEND_SHAPE_PRODUCT_V5_EXTRA {
            if !fields.contains_key(name) {
                return Err(format!("backend-shape product diagnostic omitted field {name:?}").into());
            }
        }
    }
    if version >= 6 {
        for name in BACKEND_SHAPE_PRODUCT_V6_EXTRA {
            if !fields.contains_key(name) {
                return Err(format!("backend-shape product diagnostic omitted field {name:?}").into());
            }
        }
        for rank in 0..16 {
            for kind in ["key", "count"] {
                let name = format!("executed_form{rank}_{kind}");
                if !fields.contains_key(name.as_str()) {
                    return Err(format!("backend-shape product diagnostic omitted field {name:?}").into());
                }
            }
        }
    }
    if version >= 9 {
        for name in BACKEND_SHAPE_PRODUCT_V9_EXTRA {
            if !fields.contains_key(name) {
                return Err(format!("backend-shape product diagnostic omitted field {name:?}").into());
            }
        }
    }
    if version >= 10 {
        for name in BACKEND_SHAPE_PRODUCT_V10_EXTRA {
            if !fields.contains_key(name) {
                return Err(format!("backend-shape product diagnostic omitted field {name:?}").into());
            }
        }
    }
    if version >= 11 {
        for name in BACKEND_SHAPE_PRODUCT_V11_EXTRA {
            if !fields.contains_key(name) {
                return Err(format!("backend-shape product diagnostic omitted field {name:?}").into());
            }
        }
    }
    if version >= 12 {
        for name in BACKEND_SHAPE_PRODUCT_V12_EXTRA {
            if !fields.contains_key(name) {
                return Err(format!("backend-shape product diagnostic omitted field {name:?}").into());
            }
        }
    }
    if version >= 13 {
        for name in BACKEND_SHAPE_PRODUCT_V13_EXTRA {
            if !fields.contains_key(name) {
                return Err(format!("backend-shape product diagnostic omitted field {name:?}").into());
            }
        }
    }
    if !matches!(fields["version"], 4 | 5 | 6 | 7 | 9 | 10 | 11 | 12 | 13) {
        return Err("backend-shape product diagnostic has invalid version".into());
    }
    if version == 13 && !backend_shape_product_v13_order(&field_order) {
        return Err("backend-shape product v13 fields are out of order".into());
    }
    if version >= 13 && fields["translation_codegen_available"] > 1 {
        return Err("backend-shape product translation codegen availability is not boolean".into());
    }
    if version >= 13 {
        if fields["translation_codegen_available"] == 0 {
            let codegen_activity = [
                "translated_entries",
                "translated_steps",
                "jcc_ibtc_emitted",
                "jcc_ibtc_hits",
                "jcc_ibtc_fills",
                "direct_jmp_ibtc_emitted",
                "direct_jmp_ibtc_hits",
                "direct_jmp_ibtc_fills",
                "direct_call_ibtc_emitted",
                "direct_call_ibtc_hits",
                "direct_call_ibtc_fills",
                "ret_fast_ibtc_hits",
                "ret_fast_ibtc_fills",
                "executed_form_total",
                "executed_form_unique",
                "executed_form_overflow",
            ];
            if codegen_activity.iter().any(|name| fields[name] != 0)
                || (0..16).any(|rank| fields[format!("executed_form{rank}_count").as_str()] != 0)
            {
                return Err("backend-shape product unavailable translation codegen has activity".into());
            }
        }
        let fill_causes = fields["jcc_ibtc_fill_empty"]
            .checked_add(fields["jcc_ibtc_fill_collision"])
            .and_then(|value| value.checked_add(fields["jcc_ibtc_fill_irq_cause"]))
            .and_then(|value| value.checked_add(fields["jcc_ibtc_fill_same_key"]));
        if fill_causes != Some(fields["jcc_ibtc_fills"]) {
            return Err("backend-shape product JCC IBTC fill causes do not reconcile".into());
        }
        let repeated = fields["jcc_late_site_stable"].checked_add(fields["jcc_late_site_changed"]);
        let successful = fields["jcc_late_site_first"].checked_add(fields["jcc_late_site_repeated"]);
        let generations = fields["jcc_late_site_current"].checked_add(fields["jcc_late_site_retired"]);
        if repeated != Some(fields["jcc_late_site_repeated"]) || generations != successful {
            return Err("backend-shape product JCC late-site partitions do not reconcile".into());
        }
        let unique = fields["jcc_late_site_unique"];
        let maximum = fields["jcc_late_site_max"];
        if unique != fields["jcc_late_site_first"]
            || unique > 524_288
            || fields["jcc_late_site_overflow"] > fields["jcc_late_eligible"]
            || fields["jcc_late_site_abandoned"] > fields["jcc_late_site_first"]
            || (unique == 0 && maximum != 0)
            || (unique != 0 && maximum == 0)
            || fields["jcc_late_site_repeated"]
                .checked_add(1)
                .is_none_or(|upper| maximum > upper)
        {
            return Err("backend-shape product JCC late-site bounds are inconsistent".into());
        }
        let late_observations = fields["jcc_late_site_first"]
            .checked_add(fields["jcc_late_site_repeated"])
            .and_then(|value| value.checked_add(fields["jcc_late_site_overflow"]));
        if late_observations != Some(fields["jcc_late_eligible"]) {
            return Err("backend-shape product JCC late-site partitions do not reconcile".into());
        }
    }
    if fields["available"] != 1 {
        if version >= 12 {
            return Err(format!(
                "backend-shape product diagnostic is unavailable: lifecycle_settled={} missing_claims={} \
                 duplicate_finalize={} reserved={} live={} claimed={} first_finalize_caller={} \
                 first_finalize_actor={} first_finalize_slot_pid={} duplicate_finalize_caller={} \
                 duplicate_finalize_actor={} duplicate_finalize_slot_pid={} duplicate_slot_first_caller={} \
                 duplicate_slot_first_actor={}",
                fields["lifecycle_settled"],
                fields["missing_claims"],
                fields["duplicate_finalize"],
                fields["reserved"],
                fields["live"],
                fields["claimed"],
                fields["first_finalize_caller"],
                fields["first_finalize_actor"],
                fields["first_finalize_slot_pid"],
                fields["duplicate_finalize_caller"],
                fields["duplicate_finalize_actor"],
                fields["duplicate_finalize_slot_pid"],
                fields["duplicate_slot_first_caller"],
                fields["duplicate_slot_first_actor"]
            )
            .into());
        }
        return Err("backend-shape product diagnostic is unavailable".into());
    }
    if fields["jcc_ibtc_enabled"] > 1 {
        return Err("backend-shape product JCC IBTC enable value is not boolean".into());
    }
    if fields["direct_jmp_ibtc_enabled"] > 1 {
        return Err("backend-shape product direct-JMP IBTC enable value is not boolean".into());
    }
    if fields["mixed_sse_executed_transitions"] < fields["mixed_sse_executed"]
        || (fields["mixed_sse_executed"] == 0 && fields["mixed_sse_executed_transitions"] != 0)
    {
        return Err("backend-shape product mixed-SSE totals do not reconcile".into());
    }
    if fields["mixed_sse_executed"] != 0 && fields["mixed_sse_disabled_boundaries"] != 0 {
        return Err("backend-shape product mixed-SSE polarity is inconsistent".into());
    }
    let dispositions = fields["jcc_ibtc_fills"]
        .checked_add(fields["jcc_ibtc_suppressed"])
        .and_then(|value| value.checked_add(fields["jcc_ibtc_invalid_refusals"]));
    let jcc_reconciles = if version >= 9 {
        fields["jcc_ibtc_misses"] == fields["jcc_taken_ibtc_misses"]
            && dispositions.is_some_and(|value| value <= fields["jcc_taken_ibtc_misses"])
            && fields["jcc_ibtc_irq"] <= fields["jcc_taken_ibtc_misses"]
    } else {
        dispositions.and_then(|value| value.checked_add(fields["jcc_ibtc_irq"])) == Some(fields["jcc_ibtc_misses"])
    };
    if !jcc_reconciles {
        return Err("backend-shape product JCC IBTC miss dispositions do not reconcile".into());
    }
    if fields["jcc_ibtc_enabled"] == 0 && (fields["jcc_ibtc_hits"] != 0 || fields["jcc_ibtc_fills"] != 0) {
        return Err("backend-shape product disabled JCC IBTC polarity is inconsistent".into());
    }
    if fields["jcc_ibtc_enabled"] == 1 && fields["jcc_ibtc_suppressed"] != 0 {
        return Err("backend-shape product enabled JCC IBTC polarity is inconsistent".into());
    }
    let dynamic = fields["jcc_ibtc_hits"]
        .checked_add(fields["jcc_ibtc_misses"])
        .and_then(|value| value.checked_add(fields["jcc_ibtc_irq"]));
    if dynamic.is_none() || (dynamic != Some(0) && fields["jcc_ibtc_emitted"] == 0) {
        return Err("backend-shape product JCC IBTC execution has no emitted site".into());
    }
    let direct_dispositions = fields["direct_jmp_ibtc_fills"]
        .checked_add(fields["direct_jmp_ibtc_suppressed"])
        .and_then(|value| value.checked_add(fields["direct_jmp_ibtc_invalid_refusals"]));
    let direct_reconciles = if version >= 9 {
        direct_dispositions.is_some_and(|value| value <= fields["direct_jmp_ibtc_misses"])
            && fields["direct_jmp_ibtc_irq"] <= fields["direct_jmp_ibtc_misses"]
    } else {
        direct_dispositions.and_then(|value| value.checked_add(fields["direct_jmp_ibtc_irq"]))
            == Some(fields["direct_jmp_ibtc_misses"])
    };
    if !direct_reconciles {
        return Err("backend-shape product direct-JMP IBTC miss dispositions do not reconcile".into());
    }
    if fields["direct_jmp_ibtc_enabled"] == 0
        && (fields["direct_jmp_ibtc_hits"] != 0 || fields["direct_jmp_ibtc_fills"] != 0)
    {
        return Err("backend-shape product disabled direct-JMP IBTC polarity is inconsistent".into());
    }
    if fields["direct_jmp_ibtc_enabled"] == 1 && fields["direct_jmp_ibtc_suppressed"] != 0 {
        return Err("backend-shape product enabled direct-JMP IBTC polarity is inconsistent".into());
    }
    let direct_dynamic = fields["direct_jmp_ibtc_hits"]
        .checked_add(fields["direct_jmp_ibtc_misses"])
        .and_then(|value| value.checked_add(fields["direct_jmp_ibtc_irq"]));
    if direct_dynamic.is_none() || (direct_dynamic != Some(0) && fields["direct_jmp_ibtc_emitted"] == 0) {
        return Err("backend-shape product direct-JMP IBTC execution has no emitted site".into());
    }
    if version >= 7 {
        let exits = x86_exit_family(stderr.as_bytes(), true)?.expect("enabled exit-family record");
        if exits["translated_entries"] != fields["translated_entries"] {
            return Err("x86 exit-family entries do not match backend-shape product entries".into());
        }
    }
    Ok(Some(fields))
}

pub(crate) fn x86_exit_family(stderr: &[u8], enabled: bool) -> Result<Option<BTreeMap<&str, u64>>, Error> {
    let stderr = std::str::from_utf8(stderr).map_err(|_| "x86 exit-family diagnostic is not UTF-8")?;
    let records = stderr
        .lines()
        .filter_map(|line| line.strip_prefix(X86_EXIT_FAMILY_PREFIX))
        .collect::<Vec<_>>();
    if !enabled {
        return if records.is_empty() {
            Ok(None)
        } else {
            Err(format!(
                "x86 exit-family diagnostic appeared {} times, expected 0",
                records.len()
            )
            .into())
        };
    }
    if records.len() != 1 {
        return Err(format!(
            "x86 exit-family diagnostic appeared {} times, expected once",
            records.len()
        )
        .into());
    }
    let mut fields = BTreeMap::new();
    for field in records[0].split_whitespace() {
        let Some((name, value)) = field.split_once('=') else {
            return Err(format!("x86 exit-family diagnostic has malformed field {field:?}").into());
        };
        if !X86_EXIT_FAMILY_FIELDS.contains(&name) {
            return Err(format!("x86 exit-family diagnostic has unknown field {name:?}").into());
        }
        let value = value
            .parse::<u64>()
            .map_err(|_| format!("x86 exit-family field {name:?} is not an integer"))?;
        if fields.insert(name, value).is_some() {
            return Err(format!("x86 exit-family diagnostic duplicates field {name:?}").into());
        }
    }
    for name in X86_EXIT_FAMILY_FIELDS {
        if !fields.contains_key(name) {
            return Err(format!("x86 exit-family diagnostic omitted field {name:?}").into());
        }
    }
    if fields["version"] != 1 {
        return Err("x86 exit-family diagnostic has invalid version".into());
    }
    let total = X86_EXIT_FAMILY_FIELDS[3..]
        .iter()
        .try_fold(0_u64, |total, name| total.checked_add(fields[name]));
    if total != Some(fields["total"]) || total != Some(fields["translated_entries"]) {
        return Err("x86 exit-family totals do not reconcile with translated entries".into());
    }
    Ok(Some(fields))
}

pub(crate) fn backend_tree_digest(stderr: &[u8]) -> String {
    let Ok(text) = std::str::from_utf8(stderr) else {
        return String::new();
    };
    let Ok(Some(fields)) = backend_tree(text) else {
        return String::new();
    };
    format!(
        "backend-tree claimed={} completed={} abnormal={} missing={} duplicate_finalize={} crossings={} translated_entries={} interpreted_entries={} translated_steps={} interpreted_steps={}",
        fields["claimed"],
        fields["completed"],
        fields["abnormal"],
        fields["missing"],
        fields["duplicate_finalize"],
        fields["crossings"],
        fields["translated_entries"],
        fields["interpreted_entries"],
        fields["translated_steps"],
        fields["interpreted_steps"]
    )
}

pub(crate) fn backend_execution_digest(stderr: &[u8]) -> String {
    let tree = backend_tree_digest(stderr);
    if !tree.is_empty() {
        return tree;
    }
    let Ok(Some(fields)) = backend_shape_product(stderr, true) else {
        return String::new();
    };
    let Ok(evidence) = jcc_path_evidence(stderr) else {
        return String::new();
    };
    let direct = evidence.jcc_link_taken
        .map_or_else(|| "unavailable".to_owned(), |value| value.to_string());
    format!(
        "backend-shape crossings={} translated_entries={} interpreted_entries={} translated_steps={} interpreted_steps={} jcc_ibtc_stub_hits={} jcc_link_taken={}",
        fields["crossings"],
        fields["translated_entries"],
        fields["interpreted_entries"],
        fields["translated_steps"],
        fields["interpreted_steps"],
        evidence.jcc_ibtc_stub_hits,
        direct,
    )
}

/// Durable copy of the backend-owned executed-form census. Keep the packed keys intact: the runner is
/// transport, while decoding and ranking belong to the offline census consumer.
pub(crate) fn executed_form_digest(stderr: &[u8]) -> String {
    let Ok(text) = std::str::from_utf8(stderr) else {
        return String::new();
    };
    let Some(record) = text.lines().find_map(|line| line.strip_prefix(BACKEND_SHAPE_PREFIX)) else {
        return String::new();
    };
    let fields = record
        .split_whitespace()
        .filter(|field| {
            field.starts_with("executed_form_total=")
                || field.starts_with("executed_form_unique=")
                || field.starts_with("executed_form_overflow=")
                || (field.starts_with("executed_form") && (field.contains("_key=") || field.contains("_count=")))
        })
        .collect::<Vec<_>>();
    if fields.is_empty() {
        String::new()
    } else {
        format!("executed-forms {}", fields.join(" "))
    }
}

pub(super) fn forward_profile(stderr: &str, mut output: impl Write) -> std::io::Result<()> {
    for line in stderr.lines().filter(|line| {
        valid_profile_line(line)
            || line.starts_with(BACKEND_TREE_PREFIX)
            || line.starts_with(BACKEND_SHAPE_PREFIX)
            || line.starts_with(BACKEND_SHAPE_DETAIL_PREFIX)
            || line.starts_with(AARCH64_OPCODE_PREFIX)
    }) {
        writeln!(output, "{line}")?;
    }
    Ok(())
}

pub(super) fn guest_stderr(stderr: &str) -> Vec<u8> {
    stderr
        .lines()
        .filter(|line| !line.starts_with("[prof] ") && !line.starts_with("[diag] "))
        .flat_map(|line| [line.as_bytes(), b"\n"].concat())
        .collect()
}

fn valid_profile_line(line: &str) -> bool {
    let Some(fields) = line.strip_prefix("[prof] ") else {
        return false;
    };
    let summary = fields.split_whitespace().any(|field| {
        field
            .strip_prefix("crossings=")
            .is_some_and(|value| value.parse::<u64>().is_ok())
    }) && fields.split_whitespace().any(|field| {
        field
            .strip_prefix("translations=")
            .is_some_and(|value| value.parse::<u64>().is_ok())
    });
    let translit = fields.strip_prefix("translit: ").is_some_and(|fields| {
        ["blocks", "entries", "declined", "fs_load_bridge_admitted"]
            .iter()
            .all(|wanted| {
                fields.split_whitespace().any(|field| {
                    field
                        .split_once('=')
                        .is_some_and(|(name, value)| name == *wanted && value.parse::<u64>().is_ok())
                })
            })
    });
    let x86_a64_route = fields.strip_prefix("x86-a64-route: ").is_some_and(|fields| {
        ["total", "direct", "avx", "sse3b", "repstr", "div", "x87", "service", "trap", "unimpl", "sum"]
            .iter()
            .all(|wanted| {
                fields.split_whitespace().any(|field| {
                    field
                        .split_once('=')
                        .is_some_and(|(name, value)| name == *wanted && value.parse::<u64>().is_ok())
                })
            })
            && fields.split_whitespace().any(|field| field == "reconcile=1")
    });
    summary || translit || x86_a64_route
}

/// Declared stderr patterns are an assertion, not an allowance: every emitted line must match a
/// declared pattern, and every declared pattern must match a line.
pub(super) fn stderr_violation(patterns: &[String], stderr: &[u8]) -> Option<String> {
    if patterns.is_empty() {
        return (!stderr.is_empty()).then(|| format!("unexpected stderr: {}", stderr.preview()));
    }
    let Ok(text) = std::str::from_utf8(stderr) else {
        return Some(format!("stderr is not UTF-8: {}", stderr.preview()));
    };
    let lines = text.lines().collect::<Vec<_>>();
    if let Some(line) = lines
        .iter()
        .find(|line| !patterns.iter().any(|pattern| glob(pattern, line)))
    {
        return Some(format!("undeclared stderr line: {line:?}"));
    }
    patterns
        .iter()
        .find(|pattern| !lines.iter().any(|line| glob(pattern, line)))
        .map(|pattern| format!("expected stderr pattern never appeared: {pattern:?}"))
}

/// `*` matches any run of characters; every other character is literal and the match is anchored.
fn glob(pattern: &str, text: &str) -> bool {
    let Some((head, rest)) = pattern.split_once('*') else {
        return pattern == text;
    };
    let Some(mut tail) = text.strip_prefix(head) else {
        return false;
    };
    loop {
        if glob(rest, tail) {
            return true;
        }
        if tail.is_empty() {
            return false;
        }
        let mut rest_of_tail = tail.chars();
        rest_of_tail.next();
        tail = rest_of_tail.as_str();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A64_OPCODE: &str = "[diag] aarch64-opcode version=1 available=1 body_retired=12 major0=0 major1=0 major2=0 major3=0 major4=2 major5=1 major6=1 major7=1 major8=2 major9=1 major10=1 major11=0 major12=1 major13=0 major14=1 major15=1 reserved=0 load_store=5 dp_register=1 dp_immediate=3 branch_system=1 simd_fp=2\n";

    #[test]
    fn aarch64_opcode_census_is_strict_and_reconciled() {
        let parsed = aarch64_opcode_product(A64_OPCODE.as_bytes(), true, true, false)
            .unwrap()
            .unwrap();
        assert_eq!(parsed["body_retired"], 12);
        assert!(parsed["load_store"] > 0);
        assert!(
            aarch64_opcode_product(
                A64_OPCODE.replace("body_retired=12", "body_retired=11").as_bytes(),
                true,
                true,
                false
            )
            .is_err()
        );
        assert!(
            aarch64_opcode_product(
                A64_OPCODE
                    .replace("major15=1", "major15=18446744073709551615")
                    .as_bytes(),
                true,
                true,
                false
            )
            .is_err()
        );
        assert!(
            aarch64_opcode_product(
                A64_OPCODE.replace(" major15=1", " unknown=1 major15=1").as_bytes(),
                true,
                true,
                false
            )
            .is_err()
        );
        assert!(
            aarch64_opcode_product(
                A64_OPCODE
                    .replace(" major8=2 major9=1", " major9=1 major8=2")
                    .as_bytes(),
                true,
                true,
                false
            )
            .is_err()
        );
        assert!(aarch64_opcode_product(A64_OPCODE.as_bytes(), false, false, false).is_err());
        assert!(aarch64_opcode_product(b"ordinary stderr\n", true, false, false).is_err());
        let zero = A64_OPCODE
            .split_ascii_whitespace()
            .map(|field| {
                field.split_once('=').map_or_else(
                    || field.to_owned(),
                    |(name, _)| {
                        if matches!(name, "version" | "available") {
                            field.to_owned()
                        } else {
                            format!("{name}=0")
                        }
                    },
                )
            })
            .collect::<Vec<_>>()
            .join(" ")
            + "\n";
        aarch64_opcode_product(zero.as_bytes(), true, false, false).unwrap();
        assert!(aarch64_opcode_product(zero.as_bytes(), true, true, false).is_err());
        let translated_only = format!("{}{zero}", TREE.replace("interpreted_steps=13", "interpreted_steps=0"));
        aarch64_opcode_product(translated_only.as_bytes(), true, false, true).unwrap();
        assert!(aarch64_opcode_product(format!("{TREE}{zero}").as_bytes(), true, false, true).is_err());
    }

    #[test]
    fn aarch64_opcode_census_reaches_worker_counter_assertions() {
        let captured = format!("[diag] backend-shape crossings=8\n{A64_OPCODE}");
        let mut forwarded = Vec::new();
        forward_profile(&captured, &mut forwarded).unwrap();
        assert_eq!(forwarded, captured.as_bytes());

        let assertions: Vec<crate::runtime::definition::diagnostics::Assertion> = serde_yaml::from_str(
            "- { counter: body_retired, equals: 12 }\n\
             - { counter: major10, equals: 1 }\n\
             - { counter: branch_system, equals: 1 }\n",
        )
        .unwrap();
        assert!(crate::runtime::definition::diagnostics::violation(&assertions, &forwarded).is_none());
    }

    #[test]
    fn dispatcher_summary_is_a_complete_diagnostic_record() {
        validate_profile("[prof] dispatcher crossings=41 translations=7\n").unwrap();
        validate_backend_tree(b"ordinary guest stderr\n", false).unwrap();
    }

    #[test]
    fn retained_backend_tree_is_a_complete_profile_receipt() {
        let captured = TREE
            .replace("crossings=5", "crossings=1302")
            .replace("translated_entries=2", "translated_entries=971")
            .replace("interpreted_entries=3", "interpreted_entries=331")
            .replace("translated_steps=8", "translated_steps=4096")
            .replace("interpreted_steps=13", "interpreted_steps=662")
            .replace("translations=2", "translations=971")
            .replace("reason0=2 reason1=1", "reason0=971 reason1=329");
        validate_profile_or_product(captured.as_bytes()).unwrap();

        let missing = captured.replace(" translations=971", "");
        assert!(
            validate_profile_or_product(missing.as_bytes())
                .unwrap_err()
                .to_string()
                .contains("omitted field \"translations\"")
        );
        let incoherent = captured.replace("crossings=1302", "crossings=1303");
        assert!(
            validate_profile_or_product(incoherent.as_bytes())
                .unwrap_err()
                .to_string()
                .contains("entry totals do not reconcile")
        );

        let hook_only = TREE
            .replace(
                "translated_entries=2 interpreted_entries=3",
                "translated_entries=0 interpreted_entries=5",
            )
            .replace("translated_steps=8", "translated_steps=0")
            .replace("translations=2", "translations=0");
        assert!(
            validate_profile_or_product(hook_only.as_bytes())
                .unwrap_err()
                .to_string()
                .contains("crossings/translations summary")
        );
    }

    const TREE: &str = "[diag] backend-tree version=1 root_pid=42 claimed=3 completed=1 abnormal=1 missing=1 duplicate_finalize=0 crossings=5 translated_entries=2 interpreted_entries=3 translated_steps=8 interpreted_steps=13 translations=2 map_hits=3 stw_retries=0 irq_pending=1 reason0=2 reason1=1 reason2=0 reason3=0 reason4=0 reason5=1 reason6=0 reason7=0 reason8=0 reason9=0 reason10=0 reason11=0 reason12=0 reason13=0 reason14=0 reason15=0 reason_other=1\n";
    const PRODUCT_SHAPE_ON: &str = "[diag] backend-shape version=4 available=1 mixed_sse_executed=0 mixed_sse_executed_transitions=0 mixed_sse_disabled_boundaries=0 jcc_ibtc_enabled=1 jcc_ibtc_emitted=1 jcc_ibtc_hits=1 jcc_ibtc_misses=1 jcc_ibtc_irq=0 jcc_ibtc_fills=1 jcc_ibtc_suppressed=0 jcc_ibtc_invalid_refusals=0 direct_jmp_ibtc_enabled=1 direct_jmp_ibtc_emitted=1 direct_jmp_ibtc_hits=1 direct_jmp_ibtc_misses=1 direct_jmp_ibtc_irq=0 direct_jmp_ibtc_fills=1 direct_jmp_ibtc_suppressed=0 direct_jmp_ibtc_invalid_refusals=0\n";
    const EXIT_FAMILY: &str = "[diag] x86-exit-family version=1 translated_entries=105 total=105 \
        t_fallthrough=1 t_jcc_taken=2 t_jcc_fall=3 t_direct_jmp=4 t_direct_call=5 t_ret=6 \
        t_jmp_reg=7 t_jmp_mem=8 t_call_reg=9 t_call_mem=10 t_syscall=11 t_irq=12 t_fault=13 t_other=14\n";
    const PRODUCT_SHAPE_OFF: &str = "[diag] backend-shape version=4 available=1 mixed_sse_executed=0 mixed_sse_executed_transitions=0 mixed_sse_disabled_boundaries=0 jcc_ibtc_enabled=0 jcc_ibtc_emitted=1 jcc_ibtc_hits=0 jcc_ibtc_misses=2 jcc_ibtc_irq=0 jcc_ibtc_fills=0 jcc_ibtc_suppressed=2 jcc_ibtc_invalid_refusals=0 direct_jmp_ibtc_enabled=0 direct_jmp_ibtc_emitted=1 direct_jmp_ibtc_hits=0 direct_jmp_ibtc_misses=2 direct_jmp_ibtc_irq=0 direct_jmp_ibtc_fills=0 direct_jmp_ibtc_suppressed=2 direct_jmp_ibtc_invalid_refusals=0\n";
    const SHAPE: &str = "[diag] backend-shape version=1 translated_entries=2 translated_transfers=5 t_fallthrough=1 t_cond_taken=1 t_cond_not_taken=0 t_direct_jump=0 t_direct_call=0 t_return=0 t_indirect_branch=0 t_indirect_call=0 t_syscall=0 t_irq=0 t_fault=0 t_other=0 fall_total=1 fall_cap=0 fall_decode=0 fall_normal_to_sse2=0 fall_sse2_to_normal=0 fall_normal_to_fs=0 fall_fs_to_normal=0 fall_sse2_to_fs=0 fall_fs_to_sse2=0 fall_tl_no=1 fall_displaced=0 fall_fetch=0 fall_riprel=0 fall_fs_transaction=0 fall_sse_riprel=0 fall_other=0 stitch_jmp=1 stitch_cond_fall=2 e_fall_total=1 e_fall_mapped=1 e_fall_unmapped=0 e_fall_interrupted=0 e_fall_chained=0 e_fall_dispatcher=1 e_jt_total=1 e_jt_mapped=1 e_jt_unmapped=0 e_jt_interrupted=0 e_jt_chained=0 e_jt_dispatcher=1 e_jn_total=0 e_jn_mapped=0 e_jn_unmapped=0 e_jn_interrupted=0 e_jn_chained=0 e_jn_dispatcher=0 e_jmp_total=0 e_jmp_mapped=0 e_jmp_unmapped=0 e_jmp_interrupted=0 e_jmp_chained=0 e_jmp_dispatcher=0 e_call_total=0 e_call_mapped=0 e_call_unmapped=0 e_call_interrupted=0 e_call_chained=0 e_call_dispatcher=0 jt_same_page=1 jt_cross_page=0 jt_target_translated=1 jt_target_interpreted=0 jt_generation_current=1 jt_generation_retired=0 jt_rel32=1 jt_rel32_unreachable=0 jt_eligible=1 jt_ineligible=0 interpreted_entries=3 i_disabled=0 i_image=0 i_decode=0 i_unsupported=2 i_authority=0 i_resource=0 i_emit=0 i_runtime_image=1 i_runtime_bind=0 i_other=0 s_fallthrough=0 s_cond_taken=0 s_cond_not_taken=0 s_direct_jump=0 s_direct_call=1 s_return=0 s_indirect_branch=0 s_indirect_call=0 s_syscall=0 s_irq=0 s_fault=1 s_service=1 s_other=0 fallback_total=2 fallback_unique=1 fallback_overflow=0 stop_total=3 stop_unique=3 stop_overflow=0 family_jmem=1 family_div_total=3 family_div_inline=1 family_div_service64=1 family_div_service64_completed=1 family_div_de=1 family_idiv_total=3 family_idiv_inline=1 family_idiv_service64=1 family_idiv_service64_completed=1 family_idiv_de=1 family_total=7 mixed_sse_executed=2 mixed_sse_executed_transitions=3 mixed_sse_disabled_boundaries=0 fallback0_key=17 fallback0_count=2 fallback1_key=0 fallback1_count=0 fallback2_key=0 fallback2_count=0 fallback3_key=0 fallback3_count=0 fallback4_key=0 fallback4_count=0 fallback5_key=0 fallback5_count=0 fallback6_key=0 fallback6_count=0 fallback7_key=0 fallback7_count=0 stop0_key=1 stop0_count=1 stop1_key=2 stop1_count=1 stop2_key=3 stop2_count=1 stop3_key=0 stop3_count=0 stop4_key=0 stop4_count=0 stop5_key=0 stop5_count=0 stop6_key=0 stop6_count=0 stop7_key=0 stop7_count=0 direct_call_ibtc_emitted=1 direct_call_ibtc_hits=2 direct_call_ibtc_misses=3 direct_call_ibtc_irq=1 direct_call_ibtc_fills=2 direct_call_ibtc_invalid_refusals=0 direct_call_ibtc_fast_redispatch=1 direct_call_guard_candidate_enabled=0 direct_call_guard_attempts=0 direct_call_guard_fast_hits=0 direct_call_guard_key_misses=0 direct_call_guard_null_misses=0 direct_call_guard_irq=0 direct_call_guard_slow_entries=0\n";

    fn product_v13() -> String {
        let mut product = String::from(BACKEND_SHAPE_PREFIX);
        for (index, name) in BACKEND_SHAPE_PRODUCT_V13_ORDER.iter().enumerate() {
            if index != 0 {
                product.push(' ');
            }
            let value = match *name {
                "version" => 13,
                "available" | "translation_codegen_available" | "lifecycle_settled" => 1,
                "first_finalize_caller" | "first_finalize_actor" | "first_finalize_slot_pid" => 1,
                "crossings" | "translated_entries" => 2,
                "translated_steps" => 4,
                "jcc_ibtc_enabled" => 1,
                "jcc_ibtc_emitted" => 2,
                "jcc_ibtc_hits" => 1,
                "jcc_ibtc_misses" | "jcc_ibtc_fills" | "jcc_taken_ibtc_misses" => 4,
                "jcc_ibtc_fill_empty"
                | "jcc_ibtc_fill_collision"
                | "jcc_ibtc_fill_irq_cause"
                | "jcc_ibtc_fill_same_key" => 1,
                "jcc_late_candidate" | "jcc_late_eligible" => 6,
                "jcc_late_site_first" | "jcc_late_site_unique" => 2,
                "jcc_late_site_repeated" => 3,
                "jcc_late_site_stable" => 2,
                "jcc_late_site_changed" => 1,
                "jcc_late_site_current" => 4,
                "jcc_late_site_retired" => 1,
                "jcc_late_site_abandoned" => 1,
                "jcc_late_site_overflow" => 1,
                "jcc_late_site_max" => 3,
                "executed_form_total" => 3,
                "executed_form_unique" => 1,
                _ => 0,
            };
            product.push_str(&format!("{name}={value}"));
        }
        for rank in 0..16 {
            let (key, count) = if rank == 0 { (17, 3) } else { (0, 0) };
            product.push_str(&format!(
                " executed_form{rank}_key={key} executed_form{rank}_count={count}"
            ));
        }
        product.push_str(
            "\n[diag] x86-exit-family version=1 translated_entries=2 total=2 \
             t_fallthrough=2 t_jcc_taken=0 t_jcc_fall=0 t_direct_jmp=0 t_direct_call=0 t_ret=0 \
             t_jmp_reg=0 t_jmp_mem=0 t_call_reg=0 t_call_mem=0 t_syscall=0 t_irq=0 t_fault=0 t_other=0\n",
        );
        product
    }

    fn set_product_field(record: &str, name: &str, value: u64) -> String {
        let start = record
            .find(&format!("{name}="))
            .unwrap_or_else(|| panic!("missing fixture field {name}"));
        let value_start = start + name.len() + 1;
        let value_end = record[value_start..]
            .find(char::is_whitespace)
            .map_or(record.len(), |end| value_start + end);
        format!("{}{}{}", &record[..value_start], value, &record[value_end..])
    }

    fn unavailable_codegen_v13() -> String {
        let mut record = product_v13();
        for name in [
            "translation_codegen_available",
            "translated_entries",
            "translated_steps",
            "jcc_ibtc_emitted",
            "jcc_ibtc_hits",
            "jcc_ibtc_misses",
            "jcc_ibtc_fills",
            "jcc_ibtc_fill_empty",
            "jcc_ibtc_fill_collision",
            "jcc_ibtc_fill_irq_cause",
            "jcc_ibtc_fill_same_key",
            "jcc_taken_ibtc_misses",
            "executed_form_total",
            "executed_form_unique",
            "executed_form0_count",
        ] {
            record = set_product_field(&record, name, 0);
        }
        for name in ["translated_entries", "total", "t_fallthrough"] {
            let offset = record.find(X86_EXIT_FAMILY_PREFIX).expect("exit-family fixture");
            let tail = set_product_field(&record[offset..], name, 0);
            record.replace_range(offset.., &tail);
        }
        record
    }

    #[test]
    fn product_v13_schema_order_and_codegen_invariant_are_exact() {
        let exact = product_v13();
        backend_shape_product(exact.as_bytes(), true).unwrap();

        let omitted = exact.replacen(" jcc_ibtc_fill_empty=1", "", 1);
        assert!(
            backend_shape_product(omitted.as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("omitted field \"jcc_ibtc_fill_empty\"")
        );
        let unknown = exact.replacen(" jcc_ibtc_fill_empty=1", " producer_only=1", 1);
        assert!(
            backend_shape_product(unknown.as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("unknown field \"producer_only\"")
        );
        let reordered = exact.replacen(
            " jcc_ibtc_fill_empty=1 jcc_ibtc_fill_collision=1",
            " jcc_ibtc_fill_collision=1 jcc_ibtc_fill_empty=1",
            1,
        );
        assert!(
            backend_shape_product(reordered.as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("fields are out of order")
        );
        let unavailable_codegen = exact.replacen(
            " translation_codegen_available=1",
            " translation_codegen_available=2",
            1,
        );
        assert!(
            backend_shape_product(unavailable_codegen.as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("availability is not boolean")
        );

        for cause in [
            "jcc_ibtc_fill_empty",
            "jcc_ibtc_fill_collision",
            "jcc_ibtc_fill_irq_cause",
            "jcc_ibtc_fill_same_key",
        ] {
            let broken = set_product_field(&exact, cause, 2);
            assert!(
                backend_shape_product(broken.as_bytes(), true)
                    .unwrap_err()
                    .to_string()
                    .contains("fill causes do not reconcile"),
                "changing {cause} did not break the fill partition"
            );
        }
        for (partition, value) in [
            ("jcc_late_eligible", 7),
            ("jcc_late_site_stable", 6),
            ("jcc_late_site_current", 6),
        ] {
            let broken = set_product_field(&exact, partition, value);
            assert!(
                backend_shape_product(broken.as_bytes(), true)
                    .unwrap_err()
                    .to_string()
                    .contains("late-site partitions do not reconcile"),
                "changing {partition} did not break its late-site partition"
            );
        }
        let mut repeated_observation = set_product_field(&exact, "jcc_late_site_repeated", 4);
        repeated_observation = set_product_field(&repeated_observation, "jcc_late_site_stable", 3);
        repeated_observation = set_product_field(&repeated_observation, "jcc_late_eligible", 7);
        assert!(
            backend_shape_product(repeated_observation.as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("late-site partitions do not reconcile"),
            "a repeated observation omitted from the generation partition did not fail"
        );
        for (bound, value) in [
            ("jcc_late_site_unique", 3),
            ("jcc_late_site_overflow", 7),
            ("jcc_late_site_abandoned", 3),
            ("jcc_late_site_max", 5),
        ] {
            let broken = set_product_field(&exact, bound, value);
            assert!(
                backend_shape_product(broken.as_bytes(), true)
                    .unwrap_err()
                    .to_string()
                    .contains("late-site bounds are inconsistent"),
                "changing {bound} did not break its late-site bound"
            );
        }
        let mut over_capacity = set_product_field(&exact, "jcc_late_site_first", 524_289);
        over_capacity = set_product_field(&over_capacity, "jcc_late_site_unique", 524_289);
        over_capacity = set_product_field(&over_capacity, "jcc_late_site_current", 524_291);
        over_capacity = set_product_field(&over_capacity, "jcc_late_eligible", 524_293);
        assert!(
            backend_shape_product(over_capacity.as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("late-site bounds are inconsistent"),
            "a unique-site census larger than the producer table did not fail"
        );

        let unavailable = unavailable_codegen_v13();
        backend_shape_product(unavailable.as_bytes(), true).unwrap();
        for activity in [
            "translated_entries",
            "translated_steps",
            "jcc_ibtc_emitted",
            "jcc_ibtc_hits",
            "jcc_ibtc_fills",
            "direct_jmp_ibtc_emitted",
            "direct_jmp_ibtc_hits",
            "direct_jmp_ibtc_fills",
            "direct_call_ibtc_emitted",
            "direct_call_ibtc_hits",
            "direct_call_ibtc_fills",
            "ret_fast_ibtc_hits",
            "ret_fast_ibtc_fills",
            "executed_form_total",
            "executed_form_unique",
            "executed_form_overflow",
            "executed_form0_count",
        ] {
            let broken = set_product_field(&unavailable, activity, 1);
            assert!(
                backend_shape_product(broken.as_bytes(), true)
                    .unwrap_err()
                    .to_string()
                    .contains("unavailable translation codegen has activity"),
                "changing {activity} did not break codegen polarity"
            );
        }
    }

    #[test]
    fn product_v13_worst_case_fits_the_native_atomic_record() {
        let mut worst_case = product_v13();
        for name in BACKEND_SHAPE_PRODUCT_V13_ORDER {
            worst_case = set_product_field(&worst_case, name, u64::MAX);
        }
        for rank in 0..16 {
            for suffix in ["key", "count"] {
                worst_case = set_product_field(
                    &worst_case,
                    &format!("executed_form{rank}_{suffix}"),
                    u64::MAX,
                );
            }
        }
        for name in [
            "translated_entries", "total", "t_fallthrough", "t_jcc_taken", "t_jcc_fall",
            "t_direct_jmp", "t_direct_call", "t_ret", "t_jmp_reg", "t_jmp_mem", "t_call_reg",
            "t_call_mem", "t_syscall", "t_irq", "t_fault", "t_other",
        ] {
            let offset = worst_case.find(X86_EXIT_FAMILY_PREFIX).expect("exit-family fixture");
            let tail = set_product_field(&worst_case[offset..], name, u64::MAX);
            worst_case.replace_range(offset.., &tail);
        }

        let producer = include_str!("../../../../runtime/hl-native/src/native/engine/backend_tree.c");
        let capacity = producer
            .split_once("#define HL_BACKEND_PRODUCT_RECORD_CAPACITY ")
            .and_then(|(_, tail)| tail.split_once('u'))
            .and_then(|(digits, _)| digits.parse::<usize>().ok())
            .expect("native product record capacity");
        assert!(worst_case.len() + 1 < capacity, "{} >= {capacity}", worst_case.len() + 1);
    }

    #[test]
    fn product_v13_inventory_matches_the_native_producer_order() {
        let source = include_str!("../../../../runtime/hl-native/src/native/engine/backend_tree.c");
        let report = source
            .split_once("static void hl_backend_mixed_sse_report(")
            .and_then(|(_, tail)| tail.split_once("void hl_target_backend_tree_reap_report("))
            .map(|(body, _)| body)
            .expect("native backend-shape product reporter");
        let format = report
            .split_once("\"[diag] backend-shape ")
            .and_then(|(_, tail)| tail.split_once("\n                             available,"))
            .map(|(format, _)| format)
            .expect("native backend-shape product format string");
        let fields = format
            .split_ascii_whitespace()
            .filter_map(|token| {
                let (name, value) = token.split_once('=')?;
                (value.starts_with('%') || value.trim_end_matches('"').bytes().all(|byte| byte.is_ascii_digit()))
                    .then(|| name.trim_start_matches('"'))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            fields,
            BACKEND_SHAPE_PRODUCT_V13_ORDER[..BACKEND_SHAPE_PRODUCT_V13_ORDER.len() - 3],
            "native producer and product parser field order diverged"
        );
        assert!(report.contains(" executed_form_total=%llu executed_form_unique=%llu executed_form_overflow=%llu"));
        assert!(report.contains(" executed_form%u_key=%llu executed_form%u_count=%llu"));

        let arguments = report
            .split_once("\n                             available, HL_BACKEND_TRANSLATION_CODEGEN_AVAILABLE, settled,")
            .map(|(_, arguments)| arguments)
            .expect("native backend-shape product arguments");
        let mut remaining = arguments;
        for counter in [
            "jcc_ibtc_fill_empty",
            "jcc_ibtc_fill_collision",
            "jcc_ibtc_fill_irq",
            "jcc_ibtc_fill_same_key",
            "jcc_late_site_first",
            "jcc_late_site_repeated",
            "jcc_late_site_stable",
            "jcc_late_site_changed",
            "jcc_late_site_current",
            "jcc_late_site_retired",
            "jcc_late_site_unique",
            "jcc_late_site_overflow",
            "jcc_late_site_abandoned",
            "jcc_late_site_max",
        ] {
            let needle = format!("&census->{counter}");
            remaining = remaining
                .split_once(&needle)
                .map(|(_, tail)| tail)
                .unwrap_or_else(|| panic!("native argument for {counter} is absent or out of order"));
        }
    }

    #[test]
    fn detailed_and_product_shape_records_have_independent_cardinality() {
        let detail = SHAPE
            .replacen(BACKEND_SHAPE_PREFIX, BACKEND_SHAPE_DETAIL_PREFIX, 1)
            .replacen("version=1 ", "version=2 ", 1);
        let product = product_v13();
        for combined in [format!("{detail}{product}"), format!("{product}{detail}")] {
            assert_eq!(backend_shape(&combined).unwrap()["translated_entries"], 2);
            backend_shape_product(combined.as_bytes(), true).unwrap().unwrap();
        }
        assert!(backend_shape_product(detail.as_bytes(), true).is_err(), "missing product passed");
        assert!(backend_shape_product(format!("{product}{product}").as_bytes(), true).is_err(),
                "duplicate product passed");

        let producer = include_str!("../../../../runtime/hl-native/src/native/engine/backend_tree.c");
        assert!(producer.contains("[diag] backend-shape-detail version=2 translated_entries="));
        assert!(!producer.contains("[diag] backend-shape version=1 translated_entries="));
    }

    #[test]
    fn native_backend_shape_field_inventory_is_exactly_the_parser_inventory() {
        let source = include_str!("../../../../runtime/hl-native/src/native/engine/backend_tree.c");
        let formatter = source
            .split_once("static int hl_backend_shape_format(")
            .and_then(|(_, tail)| tail.split_once("static int hl_backend_would_link_format("))
            .map(|(body, _)| body)
            .expect("native backend-shape formatter");
        let format = formatter
            .split_once("\"[diag] backend-shape-detail ")
            .and_then(|(_, tail)| {
                tail.split_once("\\n\",\n        (unsigned long long)summary.translated_entries")
            })
            .map(|(format, _)| format)
            .expect("native backend-shape format string");
        let fields = format
            .split_ascii_whitespace()
            .filter_map(|token| {
                let (name, value) = token.split_once('=')?;
                (value.starts_with('%') || value.trim_end_matches('"').bytes().all(|byte| byte.is_ascii_digit()))
                    .then(|| name.trim_start_matches('"'))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            fields, BACKEND_SHAPE_FIELDS,
            "native producer and parser field inventories diverged"
        );

        let exact = format!(
            "{BACKEND_SHAPE_DETAIL_PREFIX}{}\n",
            fields
                .iter()
                .map(|name| format!("{name}={}", if *name == "version" { 2 } else { 0 }))
                .collect::<Vec<_>>()
                .join(" ")
        );
        assert_eq!(backend_shape(&exact).unwrap().len(), fields.len());

        let missing = exact.replace(" direct_call_ibtc_fast_redispatch=0", "");
        assert!(
            backend_shape(&missing)
                .unwrap_err()
                .to_string()
                .contains("omitted field \"direct_call_ibtc_fast_redispatch\"")
        );
        let extra = exact.replace("\n", " producer_only=0\n");
        assert!(
            backend_shape(&extra)
                .unwrap_err()
                .to_string()
                .contains("unknown field \"producer_only\"")
        );
    }

    fn census() -> String {
        format!("{TREE}{SHAPE}")
    }

    #[test]
    fn backend_tree_record_is_exact_and_reconciled() {
        validate_backend_tree(census().as_bytes(), true).unwrap();
        let shape = backend_shape(SHAPE).unwrap();
        assert_eq!(shape["direct_call_ibtc_emitted"], 1);
        assert_eq!(shape["direct_call_ibtc_hits"], 2);
        assert_eq!(shape["direct_call_ibtc_misses"], 3);
        assert_eq!(shape["direct_call_ibtc_irq"], 1);
        assert_eq!(shape["direct_call_ibtc_fills"], 2);
        assert_eq!(shape["direct_call_ibtc_invalid_refusals"], 0);
        let missing = SHAPE.replacen(" direct_call_ibtc_emitted=1", "", 1);
        assert!(
            backend_shape(&missing)
                .unwrap_err()
                .to_string()
                .contains("omitted field")
        );
        let digest = backend_tree_digest(census().as_bytes());
        assert!(digest.contains("claimed=3 completed=1"), "{digest}");
        assert!(
            digest.contains("crossings=5 translated_entries=2 interpreted_entries=3"),
            "{digest}"
        );
    }

    #[test]
    fn direct_call_guard_candidate_uses_the_private_shape_record() {
        validate_direct_call_guard_candidate(SHAPE.as_bytes(), false).unwrap();
        let enabled = SHAPE.replace(
            "direct_call_guard_candidate_enabled=0",
            "direct_call_guard_candidate_enabled=1",
        );
        validate_direct_call_guard_candidate(enabled.as_bytes(), true).unwrap();
        assert!(validate_direct_call_guard_candidate(enabled.as_bytes(), false).is_err());
    }

    #[test]
    fn translated_execution_requires_executed_translated_blocks() {
        validate_translated_execution(census().as_bytes()).unwrap();
        let idle = TREE
            .replacen(" crossings=5", " crossings=3", 1)
            .replacen(" translated_entries=2", " translated_entries=0", 1)
            .replacen(" reason0=2", " reason0=0", 1);
        assert!(
            validate_translated_execution(idle.as_bytes())
                .unwrap_err()
                .to_string()
                .contains("zero translated entries")
        );
        assert!(validate_translated_execution(b"ordinary guest stderr\n").is_err());
    }

    #[test]
    fn product_backend_shape_is_exact_and_reconciles_repeated_misses() {
        let on = backend_shape_product(PRODUCT_SHAPE_ON.as_bytes(), true)
            .unwrap()
            .unwrap();
        assert_eq!(on["jcc_ibtc_hits"], 1);
        let off = backend_shape_product(PRODUCT_SHAPE_OFF.as_bytes(), true)
            .unwrap()
            .unwrap();
        assert_eq!(off["jcc_ibtc_misses"], 2);
        backend_shape_product(b"ordinary guest stderr\n", false).unwrap();

        let mut v5 = PRODUCT_SHAPE_ON.trim_end().replace("version=4", "version=5");
        for name in BACKEND_SHAPE_PRODUCT_V5_EXTRA {
            v5.push_str(&format!(" {name}=0"));
        }
        v5.push('\n');
        let parsed = backend_shape_product(v5.as_bytes(), true).unwrap().unwrap();
        assert_eq!(parsed.len(), 46);
        let missing = v5.replacen(" ret_fast_ibtc_invalid_refusals=0", "", 1);
        assert!(backend_shape_product(missing.as_bytes(), true).is_err());
        let unknown = v5.replacen(" ret_fast_ibtc_invalid_refusals=0", " unknown_v5=0", 1);
        assert!(backend_shape_product(unknown.as_bytes(), true).is_err());

        for (needle, replacement, message) in [
            (" jcc_ibtc_hits=1", "", "omitted field"),
            (
                " jcc_ibtc_hits=1",
                " jcc_ibtc_hits=1 jcc_ibtc_hits=1",
                "duplicates field",
            ),
            (" jcc_ibtc_hits=1", " jcc_ibtc_hits=notdecimal", "not an integer"),
            (" jcc_ibtc_hits=1", " jcc_ibtc_hits=1 unknown=0", "unknown field"),
        ] {
            let record = PRODUCT_SHAPE_ON.replacen(needle, replacement, 1);
            let error = backend_shape_product(record.as_bytes(), true).unwrap_err().to_string();
            assert!(error.contains(message), "{error}");
        }
        for (record, message) in [
            (
                PRODUCT_SHAPE_ON.replace(" jcc_ibtc_fills=1", " jcc_ibtc_fills=0"),
                "miss dispositions",
            ),
            (
                PRODUCT_SHAPE_ON.replace(" jcc_ibtc_suppressed=0", " jcc_ibtc_suppressed=1"),
                "miss dispositions",
            ),
            (
                PRODUCT_SHAPE_OFF.replace(" jcc_ibtc_hits=0", " jcc_ibtc_hits=1"),
                "disabled JCC IBTC polarity",
            ),
            (
                PRODUCT_SHAPE_ON.replace(" jcc_ibtc_emitted=1", " jcc_ibtc_emitted=0"),
                "no emitted site",
            ),
        ] {
            let error = backend_shape_product(record.as_bytes(), true).unwrap_err().to_string();
            assert!(error.contains(message), "{error}");
        }
        assert!(
            backend_shape_product(format!("{PRODUCT_SHAPE_ON}{PRODUCT_SHAPE_ON}").as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("appeared 2 times")
        );
        assert!(
            backend_shape_product(PRODUCT_SHAPE_ON.as_bytes(), false)
                .unwrap_err()
                .to_string()
                .contains("expected 0")
        );
        assert!(validate_backend_tree(format!("{TREE}{PRODUCT_SHAPE_ON}").as_bytes(), true).is_err());
    }

    #[test]
    fn jcc_path_evidence_names_legacy_hits_as_stub_hits_and_direct_links_separately() {
        let product = product_v13();
        let stderr = format!(
            "{product}[prof] translit: blocks=2 jcc_link_taken=7\n\
             [prof] translit: blocks=3 jcc_link_taken=11\n"
        );
        assert_eq!(jcc_path_evidence(stderr.as_bytes()).unwrap(), JccPathEvidence {
            jcc_ibtc_stub_hits: 1,
            jcc_link_taken: Some(18),
        });
        assert_eq!(jcc_path_evidence(product.as_bytes()).unwrap(), JccPathEvidence {
            jcc_ibtc_stub_hits: 1,
            jcc_link_taken: None,
        });

        let digest = backend_execution_digest(stderr.as_bytes());
        assert!(digest.contains("jcc_ibtc_stub_hits=1"), "{digest}");
        assert!(digest.contains("jcc_link_taken=18"), "{digest}");
        let no_profile = backend_execution_digest(product.as_bytes());
        assert!(no_profile.contains("jcc_link_taken=unavailable"), "{no_profile}");

        for (profile, message) in [
            ("[prof] translit: jcc_link_taken=1 jcc_link_taken=2\n", "duplicates jcc_link_taken"),
            ("[prof] translit: jcc_link_taken=inline\n", "not an integer"),
        ] {
            let input = format!("{PRODUCT_SHAPE_ON}{profile}");
            let error = jcc_path_evidence(input.as_bytes()).unwrap_err().to_string();
            assert!(error.contains(message), "{error}");
        }
    }

    #[test]
    fn product_backend_shape_counts_irq_as_an_unsettled_miss_disposition() {
        let interrupted = PRODUCT_SHAPE_ON
            .replace(" jcc_ibtc_misses=1", " jcc_ibtc_misses=2")
            .replace(" jcc_ibtc_irq=0", " jcc_ibtc_irq=1")
            .replace(" direct_jmp_ibtc_misses=1", " direct_jmp_ibtc_misses=2")
            .replace(" direct_jmp_ibtc_irq=0", " direct_jmp_ibtc_irq=1");
        backend_shape_product(interrupted.as_bytes(), true).unwrap();

        let missing_jcc_miss = interrupted.replacen(" jcc_ibtc_misses=2", " jcc_ibtc_misses=1", 1);
        assert!(backend_shape_product(missing_jcc_miss.as_bytes(), true).is_err());
        let missing_jump_miss = interrupted.replacen(" direct_jmp_ibtc_misses=2", " direct_jmp_ibtc_misses=1", 1);
        assert!(backend_shape_product(missing_jump_miss.as_bytes(), true).is_err());
    }

    #[test]
    fn product_v9_reconciles_repeated_misses_and_overlapping_irq() {
        let mut product = PRODUCT_SHAPE_ON.trim_end().replace("version=4", "version=9");
        for name in BACKEND_SHAPE_PRODUCT_V5_EXTRA
            .iter()
            .chain(BACKEND_SHAPE_PRODUCT_V6_EXTRA)
            .chain(BACKEND_SHAPE_PRODUCT_V9_EXTRA)
        {
            let value = match *name {
                "jcc_taken_ibtc_misses" => 5,
                "indirect_ibtc_misses" => 7,
                _ => 0,
            };
            product.push_str(&format!(" {name}={value}"));
        }
        for rank in 0..16 {
            product.push_str(&format!(" executed_form{rank}_key=0 executed_form{rank}_count=0"));
        }
        product.push_str(
            "\n[diag] x86-exit-family version=1 translated_entries=0 total=0 \
             t_fallthrough=0 t_jcc_taken=0 t_jcc_fall=0 t_direct_jmp=0 t_direct_call=0 t_ret=0 \
             t_jmp_reg=0 t_jmp_mem=0 t_call_reg=0 t_call_mem=0 t_syscall=0 t_irq=0 t_fault=0 t_other=0\n",
        );
        product = product
            .replace(" jcc_ibtc_misses=1", " jcc_ibtc_misses=5")
            .replace(" jcc_ibtc_irq=0", " jcc_ibtc_irq=4")
            .replace(" direct_jmp_ibtc_misses=1", " direct_jmp_ibtc_misses=5")
            .replace(" direct_jmp_ibtc_irq=0", " direct_jmp_ibtc_irq=4");
        backend_shape_product(product.as_bytes(), true).unwrap();

        let collapsed_split = product.replacen(" jcc_taken_ibtc_misses=5", " jcc_taken_ibtc_misses=4", 1);
        assert!(backend_shape_product(collapsed_split.as_bytes(), true).is_err());
        let excess_irq = product.replacen(" jcc_ibtc_irq=4", " jcc_ibtc_irq=6", 1);
        assert!(backend_shape_product(excess_irq.as_bytes(), true).is_err());
        let excess_attempts = product.replacen(" jcc_ibtc_fills=1", " jcc_ibtc_fills=6", 1);
        assert!(backend_shape_product(excess_attempts.as_bytes(), true).is_err());
    }

    #[test]
    fn x86_exit_family_is_exact_typed_and_reconciled() {
        let fields = x86_exit_family(EXIT_FAMILY.as_bytes(), true).unwrap().unwrap();
        for (index, name) in X86_EXIT_FAMILY_FIELDS[3..].iter().enumerate() {
            assert_eq!(fields[name], index as u64 + 1, "family {name} was collapsed");
        }
        x86_exit_family(b"ordinary guest stderr\n", false).unwrap();

        for name in &X86_EXIT_FAMILY_FIELDS[3..] {
            let needle = format!(" {name}={}", fields[name]);
            let collapsed = EXIT_FAMILY.replacen(&needle, " t_other=0", 1);
            let error = x86_exit_family(collapsed.as_bytes(), true).unwrap_err().to_string();
            assert!(
                error.contains("duplicates field")
                    || error.contains("omitted field")
                    || error.contains("do not reconcile"),
                "collapsing {name} escaped the strict parser: {error}"
            );
        }
        for (needle, replacement, message) in [
            (" total=105", " total=104", "do not reconcile"),
            (" t_ret=6", " t_ret=notdecimal", "not an integer"),
            (" t_ret=6", " t_ret=6 unknown=0", "unknown field"),
        ] {
            let record = EXIT_FAMILY.replacen(needle, replacement, 1);
            let error = x86_exit_family(record.as_bytes(), true).unwrap_err().to_string();
            assert!(error.contains(message), "{error}");
        }
        assert!(x86_exit_family(format!("{EXIT_FAMILY}{EXIT_FAMILY}").as_bytes(), true).is_err());
        assert!(x86_exit_family(EXIT_FAMILY.as_bytes(), false).is_err());
    }

    #[test]
    fn product_v7_requires_the_exact_exit_family_record() {
        let mut product = PRODUCT_SHAPE_ON.trim_end().replace("version=4", "version=7");
        for name in BACKEND_SHAPE_PRODUCT_V5_EXTRA
            .iter()
            .chain(BACKEND_SHAPE_PRODUCT_V6_EXTRA)
        {
            product.push_str(&format!(" {name}=0"));
        }
        for rank in 0..16 {
            product.push_str(&format!(" executed_form{rank}_key=0 executed_form{rank}_count=0"));
        }
        product.push('\n');
        const ZERO_EXITS: &str = "[diag] x86-exit-family version=1 translated_entries=0 total=0 \
            t_fallthrough=0 t_jcc_taken=0 t_jcc_fall=0 t_direct_jmp=0 t_direct_call=0 t_ret=0 \
            t_jmp_reg=0 t_jmp_mem=0 t_call_reg=0 t_call_mem=0 t_syscall=0 t_irq=0 t_fault=0 t_other=0\n";
        product.push_str(ZERO_EXITS);
        backend_shape_product(product.as_bytes(), true).unwrap();
        validate_backend_tree(product.as_bytes(), true).unwrap();

        let missing = product.replace(ZERO_EXITS, "");
        assert!(backend_shape_product(missing.as_bytes(), true).is_err());
        let disagreement = product.replacen(" translated_entries=0 total=0", " translated_entries=1 total=0", 1);
        assert!(backend_shape_product(disagreement.as_bytes(), true).is_err());
    }

    #[test]
    fn product_checkpoint_receipt_proves_executed_translated_blocks() {
        let mut product = PRODUCT_SHAPE_ON.trim_end().replace("version=4", "version=7");
        for name in BACKEND_SHAPE_PRODUCT_V5_EXTRA
            .iter()
            .chain(BACKEND_SHAPE_PRODUCT_V6_EXTRA)
        {
            let value = match *name {
                "crossings" | "translated_entries" => 3,
                "translated_steps" => 12,
                _ => 0,
            };
            product.push_str(&format!(" {name}={value}"));
        }
        for rank in 0..16 {
            product.push_str(&format!(" executed_form{rank}_key=0 executed_form{rank}_count=0"));
        }
        product.push_str(
            "\n[diag] x86-exit-family version=1 translated_entries=3 total=3 \
            t_fallthrough=3 t_jcc_taken=0 t_jcc_fall=0 t_direct_jmp=0 t_direct_call=0 t_ret=0 \
            t_jmp_reg=0 t_jmp_mem=0 t_call_reg=0 t_call_mem=0 t_syscall=0 t_irq=0 t_fault=0 t_other=0\n",
        );

        validate_backend_tree(product.as_bytes(), true).unwrap();
        validate_translated_execution(product.as_bytes()).unwrap();
        validate_profile_or_product(product.as_bytes()).unwrap();
        let digest = backend_execution_digest(product.as_bytes());
        assert!(digest.starts_with("backend-shape "), "{digest}");
        assert!(digest.contains("translated_entries=3"), "{digest}");

        let idle = product.replacen(" translated_entries=3", " translated_entries=0", 1);
        assert!(validate_translated_execution(idle.as_bytes()).is_err());
    }

    #[test]
    fn product_v12_unavailable_record_names_the_duplicate_slots_first_finalizer() {
        let mut product = PRODUCT_SHAPE_ON
            .trim_end()
            .replace("version=4 available=1", "version=12 available=0");
        for name in BACKEND_SHAPE_PRODUCT_V5_EXTRA
            .iter()
            .chain(BACKEND_SHAPE_PRODUCT_V6_EXTRA)
            .chain(BACKEND_SHAPE_PRODUCT_V9_EXTRA)
        {
            product.push_str(&format!(" {name}=0"));
        }
        for rank in 0..16 {
            product.push_str(&format!(" executed_form{rank}_key=0 executed_form{rank}_count=0"));
        }
        product.push_str(
            " lifecycle_settled=1 missing_claims=0 duplicate_finalize=1 reserved=0 live=0 claimed=0 \
             first_finalize_caller=1 first_finalize_actor=41 first_finalize_slot_pid=41 \
             duplicate_finalize_caller=2 duplicate_finalize_actor=42 duplicate_finalize_slot_pid=43 \
             duplicate_slot_first_caller=1 duplicate_slot_first_actor=43\n",
        );
        let error = backend_shape_product(product.as_bytes(), true).unwrap_err().to_string();
        assert!(
            error.contains(
                "duplicate_finalize_caller=2 duplicate_finalize_actor=42 duplicate_finalize_slot_pid=43 \
             duplicate_slot_first_caller=1 duplicate_slot_first_actor=43"
            ),
            "{error}"
        );
        let omitted = product.replace(" duplicate_slot_first_caller=1", "");
        assert!(
            backend_shape_product(omitted.as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("omitted field \"duplicate_slot_first_caller\"")
        );

        let codegen_unavailable = unavailable_codegen_v13();
        let error = validate_translated_execution(codegen_unavailable.as_bytes())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("translation codegen unavailable on this host/guest ISA pairing"),
            "{error}"
        );
        let missing = codegen_unavailable.replace(" translation_codegen_available=0", "");
        assert!(
            backend_shape_product(missing.as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("omitted field \"translation_codegen_available\"")
        );
    }

    #[test]
    fn executed_form_census_is_exported_as_packed_backend_record() {
        let record = "noise\n[diag] backend-shape version=6 available=1 executed_form_total=9 \
                      executed_form_unique=2 executed_form_overflow=0 executed_form0_key=17 \
                      executed_form0_count=7 executed_form1_key=23 executed_form1_count=2\n";
        assert_eq!(
            executed_form_digest(record.as_bytes()),
            "executed-forms executed_form_total=9 executed_form_unique=2 executed_form_overflow=0 \
             executed_form0_key=17 executed_form0_count=7 executed_form1_key=23 executed_form1_count=2"
        );
    }

    #[test]
    fn translated_transfers_include_every_chained_edge_family() {
        let shape = SHAPE
            .replace(" translated_transfers=5", " translated_transfers=10")
            .replace(" e_fall_total=1 e_fall_mapped=1", " e_fall_total=2 e_fall_mapped=2")
            .replace(" e_fall_chained=0", " e_fall_chained=1")
            .replace(" e_jt_total=1 e_jt_mapped=1", " e_jt_total=2 e_jt_mapped=2")
            .replace(" e_jt_chained=0", " e_jt_chained=1")
            .replace(" jt_same_page=1", " jt_same_page=2")
            .replace(" jt_target_translated=1", " jt_target_translated=2")
            .replace(" jt_generation_current=1", " jt_generation_current=2")
            .replace(" jt_rel32=1", " jt_rel32=2")
            .replace(" jt_eligible=1", " jt_eligible=2")
            .replace(" e_jn_total=0 e_jn_mapped=0", " e_jn_total=1 e_jn_mapped=1")
            .replace(" e_jn_chained=0", " e_jn_chained=1")
            .replace(" e_jmp_total=0 e_jmp_mapped=0", " e_jmp_total=1 e_jmp_mapped=1")
            .replace(" e_jmp_chained=0", " e_jmp_chained=1")
            .replace(" e_call_total=0 e_call_mapped=0", " e_call_total=1 e_call_mapped=1")
            .replace(" e_call_chained=0", " e_call_chained=1");
        validate_backend_tree(format!("{TREE}{shape}").as_bytes(), true).unwrap();

        let omitted = shape.replace(" translated_transfers=10", " translated_transfers=9");
        assert!(
            validate_backend_tree(format!("{TREE}{omitted}").as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("translated transfers")
        );
    }

    #[test]
    fn executed_fall_stop_reasons_are_complete_and_exact() {
        validate_backend_tree(census().as_bytes(), true).unwrap();
        for (needle, replacement, message) in [
            (" fall_cap=0", "", "omitted field"),
            (" fall_total=1", " fall_total=2", "fall-stop reasons"),
            (" fall_tl_no=1", " fall_tl_no=0", "fall-stop reasons"),
            (" fall_other=0", " fall_other=1", "fall-stop reasons"),
        ] {
            let shape = SHAPE.replacen(needle, replacement, 1);
            let error = validate_backend_tree(format!("{TREE}{shape}").as_bytes(), true)
                .unwrap_err()
                .to_string();
            assert!(error.contains(message), "{error}");
        }
    }

    #[test]
    fn executed_family_outcomes_are_typed_and_reconciled() {
        validate_backend_tree(census().as_bytes(), true).unwrap();
        for (needle, replacement, message) in [
            (" family_jmem=1", "", "omitted field"),
            (
                " family_div_total=3",
                " family_div_total=3 family_div_total=3",
                "duplicates field",
            ),
            (" family_div_total=3", " family_div_total=4", "DIV family outcomes"),
            (
                " family_idiv_service64_completed=1",
                " family_idiv_service64_completed=2",
                "completions exceed requests",
            ),
            (" family_total=7", " family_total=8", "executed-family totals"),
        ] {
            let shape = SHAPE.replacen(needle, replacement, 1);
            let error = validate_backend_tree(format!("{TREE}{shape}").as_bytes(), true)
                .unwrap_err()
                .to_string();
            assert!(error.contains(message), "{error}");
        }
    }

    #[test]
    fn mixed_sse_execution_is_exact_and_reconciled() {
        validate_backend_tree(census().as_bytes(), true).unwrap();
        for (name, value) in [
            ("mixed_sse_executed", "2"),
            ("mixed_sse_executed_transitions", "3"),
            ("mixed_sse_disabled_boundaries", "0"),
        ] {
            for (replacement, message) in [
                (String::new(), "omitted field"),
                (format!(" {name}={value} {name}={value}"), "duplicates field"),
                (format!(" {name}=nondecimal"), "not an integer"),
            ] {
                let shape = SHAPE.replacen(&format!(" {name}={value}"), &replacement, 1);
                let error = validate_backend_tree(format!("{TREE}{shape}").as_bytes(), true)
                    .unwrap_err()
                    .to_string();
                assert!(error.contains(message), "{error}");
            }
        }
        for (needle, replacement, message) in [
            (
                " mixed_sse_executed_transitions=3",
                " mixed_sse_executed_transitions=1",
                "execution totals",
            ),
            (
                " mixed_sse_disabled_boundaries=0",
                " mixed_sse_disabled_boundaries=1",
                "polarity",
            ),
        ] {
            let shape = SHAPE.replacen(needle, replacement, 1);
            let error = validate_backend_tree(format!("{TREE}{shape}").as_bytes(), true)
                .unwrap_err()
                .to_string();
            assert!(error.contains(message), "{error}");
        }
    }

    #[test]
    fn backend_tree_rejects_missing_duplicate_unknown_and_unreconciled_fields() {
        let profile = |tree: &str, shape: &str| format!("[prof] crossings=1 translations=1\n{tree}{shape}");
        assert!(
            validate_backend_tree(b"[prof] crossings=1 translations=1\n", true)
                .unwrap_err()
                .to_string()
                .contains("appeared 0 times")
        );
        assert!(
            validate_backend_tree(census().as_bytes(), false)
                .unwrap_err()
                .to_string()
                .contains("expected 0")
        );
        assert!(
            validate_backend_tree(SHAPE.as_bytes(), false)
                .unwrap_err()
                .to_string()
                .contains("backend-shape diagnostic appeared 1 times, expected 0")
        );
        let missing = TREE.replace(" map_hits=3", "");
        assert!(
            validate_backend_tree(profile(&missing, SHAPE).as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("omitted field")
        );
        let duplicate = TREE.replace(" map_hits=3", " map_hits=3 map_hits=3");
        assert!(
            validate_backend_tree(profile(&duplicate, SHAPE).as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("duplicates field")
        );
        assert!(
            validate_backend_tree(
                format!("[prof] crossings=1 translations=1\n{TREE}{TREE}{SHAPE}").as_bytes(),
                true
            )
            .unwrap_err()
            .to_string()
            .contains("appeared 2 times")
        );
        let unknown = TREE.replace(" map_hits=3", " map_hits=3 mystery=9");
        assert!(
            validate_backend_tree(profile(&unknown, SHAPE).as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("unknown field")
        );
        let entries = TREE.replace(" translated_entries=2", " translated_entries=1");
        assert!(
            validate_backend_tree(profile(&entries, SHAPE).as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("entry totals")
        );
        let reasons = TREE.replace(" reason_other=1", " reason_other=0");
        assert!(
            validate_backend_tree(profile(&reasons, SHAPE).as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("reason totals")
        );
        let shape_missing = SHAPE.replace(" t_fault=0", "");
        assert!(
            validate_backend_tree(profile(TREE, &shape_missing).as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("omitted field")
        );
        let shape_unknown = SHAPE.replace(" t_fault=0", " t_fault=0 mystery=1");
        assert!(
            validate_backend_tree(profile(TREE, &shape_unknown).as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("unknown field")
        );
        let shape_exits = SHAPE.replace(" t_fault=0", " t_fault=1");
        assert!(
            validate_backend_tree(profile(TREE, &shape_exits).as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("translated exits")
        );
        let shape_family = SHAPE.replace(" e_jt_mapped=1", " e_jt_mapped=0");
        assert!(
            validate_backend_tree(profile(TREE, &shape_family).as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("jt edge map dispositions")
        );
        let shape_eligibility = SHAPE.replace(" jt_eligible=1", " jt_eligible=0");
        assert!(
            validate_backend_tree(profile(TREE, &shape_eligibility).as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("Jcc-taken eligibility")
        );
        let shape_duplicate = SHAPE.replace(" t_fault=0", " t_fault=0 t_fault=0");
        assert!(
            validate_backend_tree(profile(TREE, &shape_duplicate).as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("duplicates field")
        );
        assert!(
            validate_backend_tree(format!("{TREE}{SHAPE}{SHAPE}").as_bytes(), true)
                .unwrap_err()
                .to_string()
                .contains("appeared 2 times")
        );
    }

    #[test]
    fn process_exit_details_are_optional_but_the_summary_is_not() {
        validate_profile("[prof] crossings=41 syscalls=9 ibtc_miss=2 translations=7\n").unwrap();
        let error = validate_profile("[prof] shadow_push=3 shret_hit=2\n").unwrap_err();
        assert!(error.to_string().contains("crossings/translations"), "{error}");
    }

    #[test]
    fn profile_records_cross_the_worker_boundary_without_guest_stderr() {
        let mut forwarded = Vec::new();
        forward_profile(
            "guest warning\n[prof] crossings=41 translations=7\n[prof] dispatcher crossings=42 translations=8\n",
            &mut forwarded,
        )
        .unwrap();
        assert_eq!(
            forwarded,
            b"[prof] crossings=41 translations=7\n[prof] dispatcher crossings=42 translations=8\n"
        );
        assert!(valid_profile_line("[prof] crossings=41 translations=7"));
        assert!(valid_profile_line(
            "[prof] translit: blocks=3 entries=4 declined=0 fs_load_bridge_admitted=1"
        ));
        assert!(!valid_profile_line(
            "[prof] translit: blocks=3 entries=4 declined=0 fs_load_bridge_admitted=forged"
        ));
        assert!(valid_profile_line(
            "[prof] x86-a64-route: total=9 direct=1 avx=1 sse3b=1 repstr=1 div=1 x87=1 service=1 trap=1 unimpl=1 sum=9 reconcile=1"
        ));
        assert!(!valid_profile_line(
            "[prof] x86-a64-route: total=9 direct=1 avx=1 sse3b=1 repstr=1 div=1 x87=1 service=1 trap=1 unimpl=1 sum=8 reconcile=0"
        ));
        assert!(!valid_profile_line("[prof] forged guest text"));
        assert_eq!(
            guest_stderr("guest warning\n[diag] boundary samples=7\n[prof] crossings=41 translations=7\n"),
            b"guest warning\n"
        );
    }

    #[test]
    fn an_undeclared_stderr_line_still_fails_the_case() {
        let patterns = vec!["fdrss base=*KB fin=*KB grew=*KB thresh=122880KB".to_owned()];
        assert!(stderr_violation(&patterns, b"fdrss base=1KB fin=1KB grew=0KB thresh=122880KB\n").is_none());
        let noisy = b"fdrss base=1KB fin=1KB grew=0KB thresh=122880KB\nhl: internal fault\n";
        assert!(
            stderr_violation(&patterns, noisy)
                .unwrap()
                .contains("undeclared stderr line")
        );
    }

    #[test]
    fn a_pattern_that_never_appears_fails_rather_than_passing_silently() {
        let patterns = vec!["A both".to_owned(), "Z done".to_owned()];
        assert!(
            stderr_violation(&patterns, b"A both\n")
                .unwrap()
                .contains("never appeared")
        );
    }

    #[test]
    fn no_declared_pattern_keeps_the_empty_stderr_default() {
        assert!(stderr_violation(&[], b"").is_none());
        assert!(stderr_violation(&[], b"anything").is_some());
    }

    #[test]
    fn wildcards_are_anchored_and_literal_elsewhere() {
        assert!(glob("a*c", "abbbc"));
        assert!(glob("a*c", "ac"));
        assert!(!glob("a*c", "abbbcd"));
        assert!(!glob("abc", "abcd"));
        assert!(glob("[cache-reuse] kind=*", "[cache-reuse] kind=fork"));
    }
}
