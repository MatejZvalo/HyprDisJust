use crate::hyprland::monitor::{slug_component, MonitorState};
use crate::profile::store::{Profile, ProfileMonitor, ProfileStore};

const EXACT_ID_SCORE: i32 = 100;
const PHYSICAL_SERIAL_SCORE: i32 = 90;
const DESCRIPTION_SCORE: i32 = 60;
const MAKE_MODEL_SCORE: i32 = 45;
const OUTPUT_NAME_SCORE: i32 = 20;
pub const HIGH_CONFIDENCE_PAIR_SCORE: i32 = DESCRIPTION_SCORE;

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
    if profile_monitors.is_empty() {
        return Vec::new();
    }

    let match_bonus = i64::try_from(profile_monitors.len())
        .unwrap_or(i64::MAX / 1_000)
        .saturating_mul(i64::from(EXACT_ID_SCORE))
        .saturating_add(1);
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
                        match_bonus + i64::from(pair.score)
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
            current[resolved.current_index].output_name,
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
                || (best.confidence.is_auto_apply_eligible()
                    && candidates
                        .iter()
                        .skip(1)
                        .any(|candidate| automatic_tie(best, candidate)))
        })
        .unwrap_or(false);

    BestProfileMatch {
        selected: if ambiguous { None } else { selected },
        candidates,
        ambiguous,
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

fn automatic_tie(left: &ProfileMatch, right: &ProfileMatch) -> bool {
    left.confidence == right.confidence
        && left.score == right.score
        && right.confidence.is_auto_apply_eligible()
}

#[derive(Debug, Clone)]
struct PairScore {
    score: i32,
    reason: &'static str,
}

fn scored_pairs(profile_monitor: &ProfileMonitor, current: &[MonitorState]) -> Vec<PairScore> {
    current
        .iter()
        .map(|monitor| {
            let (score, reason) = profile_monitor_match_score(profile_monitor, monitor);
            PairScore { score, reason }
        })
        .collect()
}

pub fn profile_monitor_match_score(
    profile_monitor: &ProfileMonitor,
    current: &MonitorState,
) -> (i32, &'static str) {
    if useful(&profile_monitor.id) && profile_monitor.id == current.id {
        return (EXACT_ID_SCORE, "exact monitor id");
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

    if same_make_model(profile_monitor, current) {
        return (MAKE_MODEL_SCORE, "make/model");
    }

    if useful(&profile_monitor.name_hint) && profile_monitor.name_hint == current.output_name {
        return (OUTPUT_NAME_SCORE, "output name hint");
    }

    (0, "no match")
}

fn same_make_model(profile_monitor: &ProfileMonitor, current: &MonitorState) -> bool {
    useful(&profile_monitor.make)
        && useful(&profile_monitor.model)
        && profile_monitor.make == current.make
        && profile_monitor.model == current.model
}

fn profile_monitor_label(profile_monitor: &ProfileMonitor) -> String {
    if useful(&profile_monitor.name_hint) {
        return profile_monitor.name_hint.clone();
    }

    profile_monitor.id.clone()
}

fn useful(value: &str) -> bool {
    !value.trim().is_empty() && slug_component(value) != "unknown"
}
