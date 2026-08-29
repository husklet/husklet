use super::{Error, diagnostic::Excerpt as _};
use std::collections::BTreeMap;
use std::io::Write;

const BACKEND_TREE_PREFIX: &str = "[diag] backend-tree ";
const BACKEND_SHAPE_PREFIX: &str = "[diag] backend-shape ";
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
const BACKEND_SHAPE_PRODUCT_V6_EXTRA: &[&str] = &[
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
    "executed_form_total",
    "executed_form_unique",
    "executed_form_overflow",
];

fn backend_shape_product_field(name: &str, schema6: bool) -> bool {
    if BACKEND_SHAPE_PRODUCT_FIELDS.contains(&name) {
        return true;
    }
    if !schema6 {
        return false;
    }
    if BACKEND_SHAPE_PRODUCT_V6_EXTRA.contains(&name) {
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

pub(super) fn validate_backend_tree(stderr: &[u8], enabled: bool) -> Result<(), Error> {
    let product = std::str::from_utf8(stderr).is_ok_and(|text| {
        text.lines()
            .filter_map(|line| line.strip_prefix(BACKEND_SHAPE_PREFIX))
            .any(|record| record.split_whitespace().any(|field| field == "version=3"))
    });
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
            .filter(|line| line.starts_with(BACKEND_SHAPE_PREFIX.as_bytes()))
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

fn backend_shape(stderr: &str) -> Result<BTreeMap<&str, u64>, Error> {
    let records = stderr
        .lines()
        .filter_map(|line| line.strip_prefix(BACKEND_SHAPE_PREFIX))
        .collect::<Vec<_>>();
    if records.len() != 1 {
        return Err(format!(
            "backend-shape diagnostic appeared {} times, expected once",
            records.len()
        )
        .into());
    }
    let mut fields = BTreeMap::new();
    for field in records[0].split_whitespace() {
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
    if fields["version"] != 1 {
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
    let schema6 = records[0].split_whitespace().any(|field| field == "version=6");
    let mut fields = BTreeMap::new();
    for field in records[0].split_whitespace() {
        let Some((name, value)) = field.split_once('=') else {
            return Err(format!("backend-shape product diagnostic has malformed field {field:?}").into());
        };
        if !backend_shape_product_field(name, schema6) {
            return Err(format!("backend-shape product diagnostic has unknown field {name:?}").into());
        }
        let value = value
            .parse::<u64>()
            .map_err(|_| format!("backend-shape product field {name:?} is not an integer"))?;
        if fields.insert(name, value).is_some() {
            return Err(format!("backend-shape product diagnostic duplicates field {name:?}").into());
        }
    }
    for name in BACKEND_SHAPE_PRODUCT_FIELDS {
        if !fields.contains_key(name) {
            return Err(format!("backend-shape product diagnostic omitted field {name:?}").into());
        }
    }
    if schema6 {
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
    if fields["version"] != 4 && fields["version"] != 6 {
        return Err("backend-shape product diagnostic has invalid version".into());
    }
    if fields["available"] != 1 {
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
    if dispositions != Some(fields["jcc_ibtc_misses"]) {
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
    if direct_dispositions != Some(fields["direct_jmp_ibtc_misses"]) {
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
        valid_profile_line(line) || line.starts_with(BACKEND_TREE_PREFIX) || line.starts_with(BACKEND_SHAPE_PREFIX)
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
    fields.split_whitespace().any(|field| {
        field
            .strip_prefix("crossings=")
            .is_some_and(|value| value.parse::<u64>().is_ok())
    }) && fields.split_whitespace().any(|field| {
        field
            .strip_prefix("translations=")
            .is_some_and(|value| value.parse::<u64>().is_ok())
    })
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

    #[test]
    fn dispatcher_summary_is_a_complete_diagnostic_record() {
        validate_profile("[prof] dispatcher crossings=41 translations=7\n").unwrap();
        validate_backend_tree(b"ordinary guest stderr\n", false).unwrap();
    }

    const TREE: &str = "[diag] backend-tree version=1 root_pid=42 claimed=3 completed=1 abnormal=1 missing=1 duplicate_finalize=0 crossings=5 translated_entries=2 interpreted_entries=3 translated_steps=8 interpreted_steps=13 translations=2 map_hits=3 stw_retries=0 irq_pending=1 reason0=2 reason1=1 reason2=0 reason3=0 reason4=0 reason5=1 reason6=0 reason7=0 reason8=0 reason9=0 reason10=0 reason11=0 reason12=0 reason13=0 reason14=0 reason15=0 reason_other=1\n";
    const PRODUCT_SHAPE_ON: &str = "[diag] backend-shape version=4 available=1 mixed_sse_executed=0 mixed_sse_executed_transitions=0 mixed_sse_disabled_boundaries=0 jcc_ibtc_enabled=1 jcc_ibtc_emitted=1 jcc_ibtc_hits=1 jcc_ibtc_misses=1 jcc_ibtc_irq=0 jcc_ibtc_fills=1 jcc_ibtc_suppressed=0 jcc_ibtc_invalid_refusals=0 direct_jmp_ibtc_enabled=1 direct_jmp_ibtc_emitted=1 direct_jmp_ibtc_hits=1 direct_jmp_ibtc_misses=1 direct_jmp_ibtc_irq=0 direct_jmp_ibtc_fills=1 direct_jmp_ibtc_suppressed=0 direct_jmp_ibtc_invalid_refusals=0\n";
    const PRODUCT_SHAPE_OFF: &str = "[diag] backend-shape version=4 available=1 mixed_sse_executed=0 mixed_sse_executed_transitions=0 mixed_sse_disabled_boundaries=0 jcc_ibtc_enabled=0 jcc_ibtc_emitted=1 jcc_ibtc_hits=0 jcc_ibtc_misses=2 jcc_ibtc_irq=0 jcc_ibtc_fills=0 jcc_ibtc_suppressed=2 jcc_ibtc_invalid_refusals=0 direct_jmp_ibtc_enabled=0 direct_jmp_ibtc_emitted=1 direct_jmp_ibtc_hits=0 direct_jmp_ibtc_misses=2 direct_jmp_ibtc_irq=0 direct_jmp_ibtc_fills=0 direct_jmp_ibtc_suppressed=2 direct_jmp_ibtc_invalid_refusals=0\n";
    const SHAPE: &str = "[diag] backend-shape version=1 translated_entries=2 translated_transfers=5 t_fallthrough=1 t_cond_taken=1 t_cond_not_taken=0 t_direct_jump=0 t_direct_call=0 t_return=0 t_indirect_branch=0 t_indirect_call=0 t_syscall=0 t_irq=0 t_fault=0 t_other=0 fall_total=1 fall_cap=0 fall_decode=0 fall_normal_to_sse2=0 fall_sse2_to_normal=0 fall_normal_to_fs=0 fall_fs_to_normal=0 fall_sse2_to_fs=0 fall_fs_to_sse2=0 fall_tl_no=1 fall_displaced=0 fall_fetch=0 fall_riprel=0 fall_fs_transaction=0 fall_sse_riprel=0 fall_other=0 stitch_jmp=1 stitch_cond_fall=2 e_fall_total=1 e_fall_mapped=1 e_fall_unmapped=0 e_fall_interrupted=0 e_fall_chained=0 e_fall_dispatcher=1 e_jt_total=1 e_jt_mapped=1 e_jt_unmapped=0 e_jt_interrupted=0 e_jt_chained=0 e_jt_dispatcher=1 e_jn_total=0 e_jn_mapped=0 e_jn_unmapped=0 e_jn_interrupted=0 e_jn_chained=0 e_jn_dispatcher=0 e_jmp_total=0 e_jmp_mapped=0 e_jmp_unmapped=0 e_jmp_interrupted=0 e_jmp_chained=0 e_jmp_dispatcher=0 e_call_total=0 e_call_mapped=0 e_call_unmapped=0 e_call_interrupted=0 e_call_chained=0 e_call_dispatcher=0 jt_same_page=1 jt_cross_page=0 jt_target_translated=1 jt_target_interpreted=0 jt_generation_current=1 jt_generation_retired=0 jt_rel32=1 jt_rel32_unreachable=0 jt_eligible=1 jt_ineligible=0 interpreted_entries=3 i_disabled=0 i_image=0 i_decode=0 i_unsupported=2 i_authority=0 i_resource=0 i_emit=0 i_runtime_image=1 i_runtime_bind=0 i_other=0 s_fallthrough=0 s_cond_taken=0 s_cond_not_taken=0 s_direct_jump=0 s_direct_call=1 s_return=0 s_indirect_branch=0 s_indirect_call=0 s_syscall=0 s_irq=0 s_fault=1 s_service=1 s_other=0 fallback_total=2 fallback_unique=1 fallback_overflow=0 stop_total=3 stop_unique=3 stop_overflow=0 family_jmem=1 family_div_total=3 family_div_inline=1 family_div_service64=1 family_div_service64_completed=1 family_div_de=1 family_idiv_total=3 family_idiv_inline=1 family_idiv_service64=1 family_idiv_service64_completed=1 family_idiv_de=1 family_total=7 mixed_sse_executed=2 mixed_sse_executed_transitions=3 mixed_sse_disabled_boundaries=0 fallback0_key=17 fallback0_count=2 fallback1_key=0 fallback1_count=0 fallback2_key=0 fallback2_count=0 fallback3_key=0 fallback3_count=0 fallback4_key=0 fallback4_count=0 fallback5_key=0 fallback5_count=0 fallback6_key=0 fallback6_count=0 fallback7_key=0 fallback7_count=0 stop0_key=1 stop0_count=1 stop1_key=2 stop1_count=1 stop2_key=3 stop2_count=1 stop3_key=0 stop3_count=0 stop4_key=0 stop4_count=0 stop5_key=0 stop5_count=0 stop6_key=0 stop6_count=0 stop7_key=0 stop7_count=0\n";

    fn census() -> String {
        format!("{TREE}{SHAPE}")
    }

    #[test]
    fn backend_tree_record_is_exact_and_reconciled() {
        validate_backend_tree(census().as_bytes(), true).unwrap();
        let digest = backend_tree_digest(census().as_bytes());
        assert!(digest.contains("claimed=3 completed=1"), "{digest}");
        assert!(
            digest.contains("crossings=5 translated_entries=2 interpreted_entries=3"),
            "{digest}"
        );
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
