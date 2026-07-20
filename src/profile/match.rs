use crate::hyprland::monitor::{
    infer_identity_provenance, slug_component, IdentityProvenance, MonitorState,
};
use crate::profile::store::{Profile, ProfileMonitor, ProfileStore};
use crate::text::sanitize_terminal_text;

const EXACT_ID_SCORE: i32 = 100;
const CORROBORATED_LEGACY_ID_SCORE: i32 = 95;
const PHYSICAL_SERIAL_SCORE: i32 = 90;
const DESCRIPTION_SCORE: i32 = 60;
const PHYSICAL_SIZE_SCORE: i32 = 50;
const MAKE_MODEL_SCORE: i32 = 45;
const OUTPUT_NAME_SCORE: i32 = 20;
pub const HIGH_CONFIDENCE_PAIR_SCORE: i32 = DESCRIPTION_SCORE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorMatchMode {
    Automatic,
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchConfidence {
    Exact,
    High,
    Partial,
    Ambiguous,
    None,
}

impl MatchConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::High => "high",
            Self::Partial => "partial",
            Self::Ambiguous => "ambiguous",
            Self::None => "none",
        }
    }

    pub fn is_auto_apply_eligible(self) -> bool {
        matches!(self, Self::Exact | Self::High)
    }

    fn rank(self) -> u8 {
        match self {
            Self::Exact => 5,
            Self::High => 4,
            Self::Ambiguous => 3,
            Self::Partial => 2,
            Self::None => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileMatch {
    pub profile_name: String,
    pub confidence: MatchConfidence,
    pub score: i32,
    pub matched_monitors: usize,
    pub profile_monitors: usize,
    pub current_monitors: usize,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BestProfileMatch {
    pub selected: Option<ProfileMatch>,
    pub candidates: Vec<ProfileMatch>,
    pub ambiguous: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoApplyDecision {
    Apply {
        profile_name: String,
        confidence: String,
        reason: String,
    },
    Ambiguous {
        reason: String,
    },
    MissingFallback {
        profile_name: String,
    },
    NoProfiles,
    NotEligible {
        reason: String,
    },
    NoMatch,
}

impl AutoApplyDecision {
    pub fn profile_name(&self) -> Option<&str> {
        match self {
            Self::Apply { profile_name, .. } => Some(profile_name),
            Self::Ambiguous { .. }
            | Self::MissingFallback { .. }
            | Self::NoProfiles
            | Self::NotEligible { .. }
            | Self::NoMatch => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMonitorMatch {
    pub current_index: usize,
    pub score: i32,
    pub reason: &'static str,
    pub ambiguous: bool,
}

pub fn resolve_monitor_matches(
    profile_monitors: &[ProfileMonitor],
    current: &[MonitorState],
) -> Vec<Option<ResolvedMonitorMatch>> {
    resolve_monitor_matches_with_mode(profile_monitors, current, MonitorMatchMode::Automatic)
}

pub fn resolve_monitor_matches_with_mode(
    profile_monitors: &[ProfileMonitor],
    current: &[MonitorState],
    mode: MonitorMatchMode,
) -> Vec<Option<ResolvedMonitorMatch>> {
    if profile_monitors.is_empty() {
        return Vec::new();
    }

    let tie_breaker_multiplier = i64::try_from(profile_monitors.len())
        .unwrap_or(i64::MAX / 1_000)
        .saturating_add(1);
    let match_bonus = i64::try_from(profile_monitors.len())
        .unwrap_or(i64::MAX / 1_000)
        .saturating_mul(i64::from(EXACT_ID_SCORE))
        .saturating_add(1)
        .saturating_mul(tie_breaker_multiplier);
    let scores: Vec<Vec<_>> = profile_monitors
        .iter()
        .map(|profile_monitor| scored_pairs(profile_monitor, current))
        .collect();
    let weights: Vec<Vec<_>> = scores
        .iter()
        .map(|row| {
            row.iter()
                .map(|pair| {
                    if pair.score > 0 {
                        let connector_tie_breaker =
                            if mode == MonitorMatchMode::Explicit && pair.connector_match {
                                1
                            } else {
                                0
                            };
                        match_bonus
                            .saturating_add(
                                i64::from(pair.score).saturating_mul(tie_breaker_multiplier),
                            )
                            .saturating_add(connector_tie_breaker)
                    } else {
                        0
                    }
                })
                .chain(std::iter::repeat_n(0, profile_monitors.len()))
                .collect()
        })
        .collect();
    let (assignment, best_total) = maximum_weight_assignment(&weights, None);

    assignment
        .into_iter()
        .enumerate()
        .map(|(profile_index, current_index)| {
            if current_index >= current.len() || scores[profile_index][current_index].score <= 0 {
                return None;
            }
            let pair = &scores[profile_index][current_index];
            let (_, alternative_total) =
                maximum_weight_assignment(&weights, Some((profile_index, current_index)));
            Some(ResolvedMonitorMatch {
                current_index,
                score: pair.score,
                reason: pair.reason,
                ambiguous: alternative_total == best_total,
            })
        })
        .collect()
}

fn maximum_weight_assignment(
    weights: &[Vec<i64>],
    banned: Option<(usize, usize)>,
) -> (Vec<usize>, i64) {
    let row_count = weights.len();
    let column_count = weights.first().map_or(0, Vec::len);
    debug_assert!(row_count <= column_count);
    let mut row_potential = vec![0_i64; row_count + 1];
    let mut column_potential = vec![0_i64; column_count + 1];
    let mut matched_row = vec![0_usize; column_count + 1];
    let mut previous_column = vec![0_usize; column_count + 1];
    const FORBIDDEN_COST: i64 = i64::MAX / 16;

    for row in 1..=row_count {
        matched_row[0] = row;
        let mut column = 0;
        let mut minimum = vec![i64::MAX; column_count + 1];
        let mut used = vec![false; column_count + 1];
        loop {
            used[column] = true;
            let active_row = matched_row[column];
            let mut delta = i64::MAX;
            let mut next_column = 0;
            for candidate in 1..=column_count {
                if used[candidate] {
                    continue;
                }
                let matrix_row = active_row - 1;
                let matrix_column = candidate - 1;
                let cost = if banned == Some((matrix_row, matrix_column)) {
                    FORBIDDEN_COST
                } else {
                    -weights[matrix_row][matrix_column]
                };
                let reduced = cost - row_potential[active_row] - column_potential[candidate];
                if reduced < minimum[candidate] {
                    minimum[candidate] = reduced;
                    previous_column[candidate] = column;
                }
                if minimum[candidate] < delta {
                    delta = minimum[candidate];
                    next_column = candidate;
                }
            }
            for candidate in 0..=column_count {
                if used[candidate] {
                    row_potential[matched_row[candidate]] += delta;
                    column_potential[candidate] -= delta;
                } else {
                    minimum[candidate] -= delta;
                }
            }
            column = next_column;
            if matched_row[column] == 0 {
                break;
            }
        }
        loop {
            let previous = previous_column[column];
            matched_row[column] = matched_row[previous];
            column = previous;
            if column == 0 {
                break;
            }
        }
    }

    let mut assignment = vec![usize::MAX; row_count];
    for column in 1..=column_count {
        if matched_row[column] != 0 {
            assignment[matched_row[column] - 1] = column - 1;
        }
    }
    let total = assignment
        .iter()
        .enumerate()
        .map(|(row, column)| weights[row][*column])
        .sum();
    (assignment, total)
}

pub fn match_profile(profile: &Profile, current: &[MonitorState]) -> ProfileMatch {
    let mut score = 0;
    let mut matched_monitors = 0;
    let mut reasons = Vec::new();
    let mut profile_is_ambiguous = false;
    let mut all_exact_ids = profile.monitors.len() == current.len();
    let mut all_high = profile.monitors.len() == current.len();

    let resolved = resolve_monitor_matches(&profile.monitors, current);
    for (profile_monitor, resolved) in profile.monitors.iter().zip(resolved) {
        let Some(resolved) = resolved else {
            reasons.push(format!(
                "{} did not match any current monitor",
                profile_monitor_label(profile_monitor)
            ));
            all_exact_ids = false;
            all_high = false;
            continue;
        };
        profile_is_ambiguous |= resolved.ambiguous;
        score += resolved.score;
        matched_monitors += 1;
        all_exact_ids &= resolved.score == EXACT_ID_SCORE;
        all_high &= resolved.score >= HIGH_CONFIDENCE_PAIR_SCORE;

        reasons.push(format!(
            "{} matched {} by {}",
            profile_monitor_label(profile_monitor),
            safe_display(&current[resolved.current_index].output_name),
            resolved.reason
        ));
    }

    let confidence = if matched_monitors == 0 {
        MatchConfidence::None
    } else if profile_is_ambiguous {
        MatchConfidence::Ambiguous
    } else if all_exact_ids {
        MatchConfidence::Exact
    } else if all_high {
        MatchConfidence::High
    } else {
        MatchConfidence::Partial
    };

    let mut summary_reasons = vec![match confidence {
        MatchConfidence::Exact => format!(
            "{matched_monitors}/{} monitor identities matched exactly",
            profile.monitors.len()
        ),
        MatchConfidence::High => format!(
            "{matched_monitors}/{} monitor identities matched with high confidence",
            profile.monitors.len()
        ),
        MatchConfidence::Partial => format!(
            "{matched_monitors}/{} profile monitors matched; automatic apply is not eligible",
            profile.monitors.len()
        ),
        MatchConfidence::Ambiguous => {
            "multiple current monitors matched equally; automatic apply is not eligible".to_owned()
        }
        MatchConfidence::None => "no monitor identities matched".to_owned(),
    }];
    summary_reasons.extend(reasons);

    ProfileMatch {
        profile_name: profile.name.clone(),
        confidence,
        score,
        matched_monitors,
        profile_monitors: profile.monitors.len(),
        current_monitors: current.len(),
        reasons: summary_reasons,
    }
}

pub fn best_profile_match(store: &ProfileStore, current: &[MonitorState]) -> BestProfileMatch {
    let mut candidates: Vec<_> = store
        .profiles
        .iter()
        .map(|profile| match_profile(profile, current))
        .collect();
    candidates.sort_by(compare_profile_matches);

    let selected = candidates.first().cloned().filter(|candidate| {
        !matches!(
            candidate.confidence,
            MatchConfidence::Ambiguous | MatchConfidence::None
        )
    });

    let ambiguous = candidates
        .first()
        .map(|best| {
            best.confidence == MatchConfidence::Ambiguous
                || candidates
                    .iter()
                    .skip(1)
                    .any(|candidate| profile_tie(best, candidate))
        })
        .unwrap_or(false);

    BestProfileMatch {
        selected: if ambiguous { None } else { selected },
        candidates,
        ambiguous,
    }
}

pub fn decide_auto_apply(
    store: &ProfileStore,
    best_match: &BestProfileMatch,
    fallback_profile: Option<&str>,
) -> AutoApplyDecision {
    if store.profiles.is_empty() {
        return AutoApplyDecision::NoProfiles;
    }

    if let Some(selected) = best_match
        .selected
        .as_ref()
        .filter(|selected| selected.confidence.is_auto_apply_eligible())
    {
        return AutoApplyDecision::Apply {
            profile_name: selected.profile_name.clone(),
            confidence: selected.confidence.as_str().to_owned(),
            reason: first_reason(&selected.reasons, "profile matched"),
        };
    }

    if best_match.ambiguous {
        return AutoApplyDecision::Ambiguous {
            reason: best_ambiguous_reason(best_match),
        };
    }

    if let Some(fallback_profile) = normalized_fallback_profile(fallback_profile) {
        if store.has_profile(fallback_profile) {
            return AutoApplyDecision::Apply {
                profile_name: fallback_profile.to_owned(),
                confidence: "fallback".to_owned(),
                reason: "no exact or high-confidence match; fallback_profile is configured"
                    .to_owned(),
            };
        }

        return AutoApplyDecision::MissingFallback {
            profile_name: fallback_profile.to_owned(),
        };
    }

    if let Some(selected) = &best_match.selected {
        return AutoApplyDecision::NotEligible {
            reason: first_reason(
                &selected.reasons,
                "profile match is not eligible for automatic apply",
            ),
        };
    }

    AutoApplyDecision::NoMatch
}

pub fn format_auto_apply_decision(decision: &AutoApplyDecision, profile_label: &str) -> String {
    match decision {
        AutoApplyDecision::Apply {
            profile_name,
            confidence,
            reason,
        } => {
            format!("{profile_label}: {profile_name}\nConfidence: {confidence}\nReason: {reason}")
        }
        AutoApplyDecision::Ambiguous { reason } => {
            format!("{profile_label}: ambiguous\nConfidence: ambiguous\nReason: {reason}")
        }
        AutoApplyDecision::MissingFallback { profile_name } => format!(
            "{profile_label}: none\nConfidence: none\nReason: fallback_profile `{profile_name}` does not exist"
        ),
        AutoApplyDecision::NoProfiles => {
            format!("{profile_label}: none\nConfidence: none\nReason: no profiles saved")
        }
        AutoApplyDecision::NotEligible { reason } => {
            format!("{profile_label}: none\nConfidence: none\nReason: {reason}")
        }
        AutoApplyDecision::NoMatch => {
            format!("{profile_label}: none\nConfidence: none\nReason: no useful profile match")
        }
    }
}

fn compare_profile_matches(left: &ProfileMatch, right: &ProfileMatch) -> std::cmp::Ordering {
    right
        .confidence
        .rank()
        .cmp(&left.confidence.rank())
        .then_with(|| right.score.cmp(&left.score))
        .then_with(|| right.matched_monitors.cmp(&left.matched_monitors))
        .then_with(|| left.profile_name.cmp(&right.profile_name))
}

fn profile_tie(left: &ProfileMatch, right: &ProfileMatch) -> bool {
    left.confidence == right.confidence
        && left.score == right.score
        && left.matched_monitors == right.matched_monitors
        && left.confidence != MatchConfidence::None
}

#[derive(Debug, Clone)]
struct PairScore {
    score: i32,
    reason: &'static str,
    connector_match: bool,
}

fn scored_pairs(profile_monitor: &ProfileMonitor, current: &[MonitorState]) -> Vec<PairScore> {
    current
        .iter()
        .map(|monitor| {
            let (score, reason) = profile_monitor_match_score(profile_monitor, monitor);
            PairScore {
                score,
                reason,
                connector_match: useful(&profile_monitor.name_hint)
                    && profile_monitor.name_hint == monitor.output_name,
            }
        })
        .collect()
}

pub fn profile_monitor_match_score(
    profile_monitor: &ProfileMonitor,
    current: &MonitorState,
) -> (i32, &'static str) {
    if trusted_exact_identity(profile_monitor, current) {
        return (EXACT_ID_SCORE, "exact monitor id");
    }

    if corroborated_legacy_identity(profile_monitor, current) {
        return (CORROBORATED_LEGACY_ID_SCORE, "exact monitor id");
    }

    if same_make_model(profile_monitor, current)
        && useful(&profile_monitor.serial)
        && profile_monitor.serial == current.serial
    {
        return (PHYSICAL_SERIAL_SCORE, "make/model/serial");
    }

    if useful(&profile_monitor.description) && profile_monitor.description == current.description {
        return (DESCRIPTION_SCORE, "exact description");
    }

    if same_make_model(profile_monitor, current) && same_physical_size(profile_monitor, current) {
        return (PHYSICAL_SIZE_SCORE, "make/model/physical size");
    }

    if same_make_model(profile_monitor, current) {
        return (MAKE_MODEL_SCORE, "make/model");
    }

    if useful(&profile_monitor.name_hint) && profile_monitor.name_hint == current.output_name {
        return (OUTPUT_NAME_SCORE, "output name hint");
    }

    (0, "no match")
}

fn corroborated_legacy_identity(profile_monitor: &ProfileMonitor, current: &MonitorState) -> bool {
    if profile_monitor.id != current.id
        || infer_identity_provenance(
            &profile_monitor.id,
            &profile_monitor.make,
            &profile_monitor.model,
            &profile_monitor.serial,
            &profile_monitor.description,
        ) != IdentityProvenance::LegacyUntrusted
    {
        return false;
    }
    (same_make_model(profile_monitor, current)
        && profile_monitor.serial == current.serial
        && useful(&profile_monitor.make)
        && useful(&profile_monitor.model))
        || (useful(&profile_monitor.description)
            && profile_monitor.description == current.description)
}

fn trusted_exact_identity(profile_monitor: &ProfileMonitor, current: &MonitorState) -> bool {
    if !useful(&profile_monitor.id) || profile_monitor.id != current.id {
        return false;
    }
    let stored_provenance = infer_identity_provenance(
        &profile_monitor.id,
        &profile_monitor.make,
        &profile_monitor.model,
        &profile_monitor.serial,
        &profile_monitor.description,
    );
    if stored_provenance != current.identity_provenance() {
        return false;
    }
    match stored_provenance {
        IdentityProvenance::PhysicalSerial => {
            profile_monitor.make == current.make
                && profile_monitor.model == current.model
                && profile_monitor.serial == current.serial
        }
        IdentityProvenance::PhysicalNoSerial => {
            profile_monitor.make == current.make && profile_monitor.model == current.model
        }
        IdentityProvenance::Description => profile_monitor.description == current.description,
        IdentityProvenance::ConnectorFallback
        | IdentityProvenance::ConnectorDisambiguated
        | IdentityProvenance::LegacyUntrusted => false,
    }
}

fn same_make_model(profile_monitor: &ProfileMonitor, current: &MonitorState) -> bool {
    useful(&profile_monitor.make)
        && useful(&profile_monitor.model)
        && profile_monitor.make == current.make
        && profile_monitor.model == current.model
}

fn same_physical_size(profile_monitor: &ProfileMonitor, current: &MonitorState) -> bool {
    profile_monitor.physical_width > 0
        && profile_monitor.physical_height > 0
        && profile_monitor.physical_width == current.physical_width
        && profile_monitor.physical_height == current.physical_height
}

fn profile_monitor_label(profile_monitor: &ProfileMonitor) -> String {
    if useful(&profile_monitor.name_hint) {
        return safe_display(&profile_monitor.name_hint);
    }

    safe_display(&profile_monitor.id)
}

fn useful(value: &str) -> bool {
    !value.trim().is_empty() && slug_component(value) != "unknown"
}

fn safe_display(value: &str) -> String {
    sanitize_terminal_text(value)
}

fn normalized_fallback_profile(fallback_profile: Option<&str>) -> Option<&str> {
    fallback_profile
        .map(str::trim)
        .filter(|fallback_profile| !fallback_profile.is_empty())
}

fn best_ambiguous_reason(best_match: &BestProfileMatch) -> String {
    best_match
        .candidates
        .first()
        .and_then(|candidate| candidate.reasons.first())
        .map(String::as_str)
        .unwrap_or("multiple profiles or monitors matched equally")
        .to_owned()
}

fn first_reason(reasons: &[String], fallback: &str) -> String {
    reasons
        .first()
        .map(String::as_str)
        .unwrap_or(fallback)
        .to_owned()
}
