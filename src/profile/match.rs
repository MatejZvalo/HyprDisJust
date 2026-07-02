use crate::hyprland::monitor::{slug_component, MonitorState};
use crate::profile::store::{Profile, ProfileMonitor, ProfileStore};

const EXACT_ID_SCORE: i32 = 100;
const PHYSICAL_SERIAL_SCORE: i32 = 90;
const DESCRIPTION_SCORE: i32 = 60;
const MAKE_MODEL_SCORE: i32 = 45;
const OUTPUT_NAME_SCORE: i32 = 20;

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
            Self::Partial => 3,
            Self::Ambiguous => 2,
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

pub fn match_profile(profile: &Profile, current: &[MonitorState]) -> ProfileMatch {
    let mut used_current = vec![false; current.len()];
    let mut score = 0;
    let mut matched_monitors = 0;
    let mut reasons = Vec::new();
    let mut profile_is_ambiguous = false;
    let mut all_exact_ids = profile.monitors.len() == current.len();
    let mut all_high = profile.monitors.len() == current.len();

    for profile_monitor in &profile.monitors {
        let pair_scores = scored_pairs(profile_monitor, current);
        let available_pair_scores: Vec<_> = pair_scores
            .into_iter()
            .filter(|pair_score| !used_current[pair_score.current_index])
            .collect();

        let Some(best_score) = available_pair_scores
            .iter()
            .map(|pair_score| pair_score.score)
            .max()
        else {
            reasons.push(format!(
                "{} did not match any current monitor",
                profile_monitor_label(profile_monitor)
            ));
            all_exact_ids = false;
            all_high = false;
            continue;
        };

        if best_score <= 0 {
            reasons.push(format!(
                "{} did not match any current monitor",
                profile_monitor_label(profile_monitor)
            ));
            all_exact_ids = false;
            all_high = false;
            continue;
        }

        let best_pairs: Vec<_> = available_pair_scores
            .iter()
            .filter(|pair_score| pair_score.score == best_score)
            .collect();
        if best_pairs.len() > 1 {
            profile_is_ambiguous = true;
        }

        let best_pair = best_pairs
            .into_iter()
            .min_by(|left, right| {
                let left_monitor = &current[left.current_index];
                let right_monitor = &current[right.current_index];
                left_monitor.output_name.cmp(&right_monitor.output_name)
            })
            .expect("best pair should exist when best_score exists");

        used_current[best_pair.current_index] = true;
        score += best_pair.score;
        matched_monitors += 1;
        all_exact_ids &= best_pair.score == EXACT_ID_SCORE;
        all_high &= best_pair.score >= DESCRIPTION_SCORE;

        reasons.push(format!(
            "{} matched {} by {}",
            profile_monitor_label(profile_monitor),
            current[best_pair.current_index].output_name,
            best_pair.reason
        ));
    }

    if matched_monitors < profile.monitors.len() {
        profile_is_ambiguous |= profile.monitors.len() > current.len();
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
    current_index: usize,
    score: i32,
    reason: &'static str,
}

fn scored_pairs(profile_monitor: &ProfileMonitor, current: &[MonitorState]) -> Vec<PairScore> {
    current
        .iter()
        .enumerate()
        .map(|(current_index, monitor)| {
            let (score, reason) = monitor_pair_score(profile_monitor, monitor);
            PairScore {
                current_index,
                score,
                reason,
            }
        })
        .collect()
}

fn monitor_pair_score(
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
