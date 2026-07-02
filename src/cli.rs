use std::env;
use std::fs;

use anyhow::bail;
use clap::{Parser, Subcommand, ValueEnum};

use crate::config::{AppConfig, ConfigPaths};
use crate::hyprland::hyprctl::parse_monitors_output;
use crate::hyprland::hyprctl::HyprctlClient;
use crate::hyprland::monitor::MonitorState;
use crate::profile::r#match::{best_profile_match, BestProfileMatch, ProfileMatch};
use crate::profile::render::{format_hyprctl_batch_command, render_monitor_rules};
use crate::profile::store::Profile;
use crate::profile::store::ProfileStore;

const MONITORS_JSON_ENV: &str = "HYPRDISJUST_MONITORS_JSON";

#[derive(Debug, Parser)]
#[command(name = "hyprdisjust")]
#[command(about = "Hyprland monitor profile manager")]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Inspect the current Hyprland monitor state.
    Doctor,
    /// List saved monitor profiles.
    List,
    /// Save the current monitor layout as a profile.
    Save {
        /// Profile name. A default name will be chosen later when omitted.
        name: Option<String>,
        /// Replace an existing profile with the same name.
        #[arg(long)]
        replace: bool,
    },
    /// Apply a saved profile or choose one automatically.
    Apply {
        /// Profile name to apply.
        name: Option<String>,
        /// Select the best matching profile for the current monitor set.
        #[arg(long)]
        auto: bool,
        /// Explain what would be selected without changing monitor layout.
        #[arg(long)]
        dry_run: bool,
    },
    /// Run the hotplug listener daemon.
    Daemon,
    /// Export generated Hyprland monitor configuration.
    Export {
        /// Export format.
        #[arg(long, value_enum)]
        format: ExportFormat,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ExportFormat {
    Conf,
    Lua,
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Doctor => {
            let paths = ConfigPaths::resolve()?;
            let monitors = current_monitors()?;
            let store = ProfileStore::load(paths.profile_store_path())?;
            let config = AppConfig::load(paths.config_file_path())?;
            let mut output = format_doctor(&monitors);
            if let Some(summary) = format_best_profile_summary(
                &best_profile_match(&store, &monitors),
                &store,
                config.fallback_profile.as_deref(),
            ) {
                output.push_str("\n\n");
                output.push_str(&summary);
            }
            println!("{output}");
        }
        Commands::List => {
            let paths = ConfigPaths::resolve()?;
            let store = ProfileStore::load(paths.profile_store_path())?;
            println!("{}", format_profile_list(&store));
        }
        Commands::Save { name, replace } => {
            let paths = ConfigPaths::resolve()?;
            let monitors = current_monitors()?;
            let mut store = ProfileStore::load(paths.profile_store_path())?;
            let saved_name = store.save_current_profile(name.as_deref(), &monitors, replace)?;
            store.save_atomic(paths.profile_store_path())?;
            println!(
                "Saved profile `{}` with {} monitor{}.",
                saved_name,
                monitors.len(),
                if monitors.len() == 1 { "" } else { "s" }
            );
        }
        Commands::Apply {
            name,
            auto,
            dry_run,
        } => run_apply(name.as_deref(), auto, dry_run)?,
        Commands::Daemon => not_implemented("daemon")?,
        Commands::Export { .. } => not_implemented("export")?,
    }

    Ok(())
}

fn not_implemented(command: &str) -> anyhow::Result<()> {
    bail!("`{command}` is not implemented yet")
}

fn run_apply(name: Option<&str>, auto: bool, dry_run: bool) -> anyhow::Result<()> {
    if name.is_some() && auto {
        bail!("pass either a profile name or --auto, not both");
    }

    let paths = ConfigPaths::resolve()?;
    let store = ProfileStore::load(paths.profile_store_path())?;

    if let Some(name) = name {
        let profile = profile_by_name(&store, name)?;
        let monitors = current_monitors()?;
        let rendered = render_monitor_rules(profile, &monitors)?;
        apply_or_print(profile, &rendered.batch, &monitors, dry_run)?;
        return Ok(());
    }

    if !auto {
        bail!("`apply` requires a profile name or --auto");
    }

    let monitors = current_monitors()?;
    let config = AppConfig::load(paths.config_file_path())?;
    let best_match = best_profile_match(&store, &monitors);

    if dry_run {
        let mut output =
            format_auto_apply_dry_run(&best_match, &store, config.fallback_profile.as_deref());
        if let Some(profile) =
            auto_profile_candidate(&store, &best_match, config.fallback_profile.as_deref())
        {
            let rendered = render_monitor_rules(profile, &monitors)?;
            output.push_str("\n\n");
            output.push_str(&format_apply_commands(profile, &rendered.batch));
        } else {
            output.push_str("\n\nCommands: none");
        }
        println!("{output}");
        return Ok(());
    }

    let profile = require_auto_profile(&store, &best_match, config.fallback_profile.as_deref())?;
    let rendered = render_monitor_rules(profile, &monitors)?;
    apply_or_print(profile, &rendered.batch, &monitors, dry_run)
}

fn current_monitors() -> anyhow::Result<Vec<MonitorState>> {
    if let Some(path) = env::var_os(MONITORS_JSON_ENV) {
        let contents = fs::read_to_string(&path)?;
        return parse_monitors_output(&contents);
    }

    let client = HyprctlClient;
    client.monitors_all()
}

fn profile_by_name<'a>(store: &'a ProfileStore, name: &str) -> anyhow::Result<&'a Profile> {
    store
        .profiles
        .iter()
        .find(|profile| profile.name == name)
        .ok_or_else(|| anyhow::anyhow!("profile `{name}` does not exist"))
}

fn auto_profile_candidate<'a>(
    store: &'a ProfileStore,
    best_match: &BestProfileMatch,
    fallback_profile: Option<&str>,
) -> Option<&'a Profile> {
    if let Some(selected) = best_match
        .selected
        .as_ref()
        .filter(|selected| selected.confidence.is_auto_apply_eligible())
    {
        return profile_by_name(store, &selected.profile_name).ok();
    }

    if best_match.ambiguous && has_auto_eligible_candidate(best_match) {
        return None;
    }

    normalized_fallback_profile(fallback_profile).and_then(|fallback_profile| {
        store
            .profiles
            .iter()
            .find(|profile| profile.name == fallback_profile)
    })
}

fn require_auto_profile<'a>(
    store: &'a ProfileStore,
    best_match: &BestProfileMatch,
    fallback_profile: Option<&str>,
) -> anyhow::Result<&'a Profile> {
    if let Some(profile) = auto_profile_candidate(store, best_match, fallback_profile) {
        return Ok(profile);
    }

    if store.profiles.is_empty() {
        bail!("no profiles saved");
    }

    if best_match.ambiguous && has_auto_eligible_candidate(best_match) {
        let reason = best_match
            .candidates
            .first()
            .and_then(|candidate| candidate.reasons.first())
            .map(String::as_str)
            .unwrap_or("multiple profiles or monitors matched equally");
        bail!("automatic apply is ambiguous: {reason}");
    }

    if let Some(fallback_profile) = normalized_fallback_profile(fallback_profile) {
        bail!("fallback_profile `{fallback_profile}` does not exist");
    }

    if let Some(selected) = &best_match.selected {
        let reason = selected
            .reasons
            .first()
            .map(String::as_str)
            .unwrap_or("profile match is not eligible for automatic apply");
        bail!("no exact or high-confidence profile match: {reason}");
    }

    bail!("no useful profile match")
}

fn apply_or_print(
    profile: &Profile,
    batch: &str,
    previous_monitors: &[MonitorState],
    dry_run: bool,
) -> anyhow::Result<()> {
    if dry_run {
        println!("{}", format_apply_commands(profile, batch));
        return Ok(());
    }

    let client = HyprctlClient;
    if let Err(error) = client.apply_monitor_batch(batch) {
        bail!(
            "{error}\nPrevious layout:\n{}",
            format_doctor(previous_monitors)
        );
    }

    println!(
        "Applied profile `{}` with {} monitor rule{}.",
        profile.name,
        profile.outputs.len(),
        if profile.outputs.len() == 1 { "" } else { "s" }
    );
    Ok(())
}

fn format_apply_commands(profile: &Profile, batch: &str) -> String {
    format!(
        "Profile: {}\nCommand:\n{}",
        profile.name,
        format_hyprctl_batch_command(batch)
    )
}

pub fn format_auto_apply_dry_run(
    best_match: &BestProfileMatch,
    store: &ProfileStore,
    fallback_profile: Option<&str>,
) -> String {
    let mut output = "Auto-apply dry run".to_owned();
    output.push('\n');
    output.push_str(&format_match_decision(
        best_match,
        store,
        fallback_profile,
        "Would select profile",
    ));
    output
}

pub fn format_best_profile_summary(
    best_match: &BestProfileMatch,
    store: &ProfileStore,
    fallback_profile: Option<&str>,
) -> Option<String> {
    if store.profiles.is_empty() {
        return None;
    }

    Some(format_match_decision(
        best_match,
        store,
        fallback_profile,
        "Best profile",
    ))
}

fn format_match_decision(
    best_match: &BestProfileMatch,
    store: &ProfileStore,
    fallback_profile: Option<&str>,
    profile_label: &str,
) -> String {
    if store.profiles.is_empty() {
        return format!("{profile_label}: none\nConfidence: none\nReason: no profiles saved");
    }

    if let Some(selected) = best_match
        .selected
        .as_ref()
        .filter(|selected| selected.confidence.is_auto_apply_eligible())
    {
        return format_profile_match(profile_label, selected);
    }

    if best_match.ambiguous && has_auto_eligible_candidate(best_match) {
        let reason = best_match
            .candidates
            .first()
            .and_then(|candidate| candidate.reasons.first())
            .map(String::as_str)
            .unwrap_or("multiple profiles or monitors matched equally");
        return format!("{profile_label}: ambiguous\nConfidence: ambiguous\nReason: {reason}");
    }

    if let Some(fallback_profile) = normalized_fallback_profile(fallback_profile) {
        if store.has_profile(fallback_profile) {
            return format!(
                "{profile_label}: {fallback_profile}\nConfidence: fallback\nReason: no exact or high-confidence match; fallback_profile is configured"
            );
        }

        return format!(
            "{profile_label}: none\nConfidence: none\nReason: fallback_profile `{fallback_profile}` does not exist"
        );
    }

    if best_match.ambiguous {
        let reason = best_match
            .candidates
            .first()
            .and_then(|candidate| candidate.reasons.first())
            .map(String::as_str)
            .unwrap_or("multiple profiles or monitors matched equally");
        return format!("{profile_label}: ambiguous\nConfidence: ambiguous\nReason: {reason}");
    }

    if let Some(selected) = &best_match.selected {
        return format_profile_match(profile_label, selected);
    }

    format!("{profile_label}: none\nConfidence: none\nReason: no useful profile match")
}

fn has_auto_eligible_candidate(best_match: &BestProfileMatch) -> bool {
    best_match
        .candidates
        .iter()
        .any(|candidate| candidate.confidence.is_auto_apply_eligible())
}

fn normalized_fallback_profile(fallback_profile: Option<&str>) -> Option<&str> {
    fallback_profile
        .map(str::trim)
        .filter(|fallback_profile| !fallback_profile.is_empty())
}

fn format_profile_match(profile_label: &str, profile_match: &ProfileMatch) -> String {
    let reason = profile_match
        .reasons
        .first()
        .map(String::as_str)
        .unwrap_or("profile matched");

    format!(
        "{profile_label}: {}\nConfidence: {}\nReason: {reason}",
        profile_match.profile_name,
        profile_match.confidence.as_str()
    )
}

pub fn format_profile_list(store: &ProfileStore) -> String {
    if store.profiles.is_empty() {
        return "No profiles saved yet.".to_owned();
    }

    let mut output = format!("Profiles: {}", store.profiles.len());
    for profile in &store.profiles {
        output.push_str(&format!(
            "\n- {} ({} monitor{})",
            profile.name,
            profile.monitors.len(),
            if profile.monitors.len() == 1 { "" } else { "s" }
        ));
    }

    output
}

pub fn format_doctor(monitors: &[MonitorState]) -> String {
    let mut output = format!("Hyprland: detected\nMonitors: {}", monitors.len());

    for (index, monitor) in monitors.iter().enumerate() {
        output.push_str("\n\n");
        output.push_str(&format!("{}. {}\n", index + 1, monitor.output_name));
        output.push_str(&format!("   id: {}\n", monitor.id));
        output.push_str(&format!("   description: {}\n", monitor.description));
        output.push_str(&format!(
            "   mode: {}x{}@{}\n",
            monitor.width,
            monitor.height,
            format_number(monitor.refresh_rate)
        ));
        output.push_str(&format!("   position: {}x{}\n", monitor.x, monitor.y));
        output.push_str(&format!("   scale: {}", format_number(monitor.scale)));
        if !monitor.enabled {
            output.push_str("\n   status: disabled");
        }
    }

    output
}

fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        return format!("{value:.0}");
    }

    let formatted = format!("{value:.3}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}
