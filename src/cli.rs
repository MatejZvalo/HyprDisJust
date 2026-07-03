use std::path::PathBuf;

use anyhow::bail;
use clap::{Parser, Subcommand, ValueEnum};

use crate::config::{write_generated_file, AppConfig, ConfigPaths};
use crate::daemon::{
    decide_auto_apply, format_auto_apply_decision, AutoApplyDecision, DaemonOptions,
};
use crate::hyprland::hyprctl::current_monitors;
use crate::hyprland::hyprctl::HyprctlClient;
use crate::hyprland::monitor::MonitorState;
use crate::profile::r#match::{best_profile_match, BestProfileMatch};
use crate::profile::render::{
    format_hyprctl_batch_command, render_hyprland_conf, render_hyprland_lua, render_monitor_rules,
};
use crate::profile::store::Profile;
use crate::profile::store::ProfileStore;

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
    Daemon {
        /// Run one auto-apply decision and exit.
        #[arg(long)]
        once: bool,
        /// Explain what would be selected without changing monitor layout.
        #[arg(long)]
        dry_run: bool,
        /// Append daemon logs to a file as well as stdout.
        #[arg(long)]
        log_file: Option<PathBuf>,
    },
    /// Export generated Hyprland monitor configuration.
    Export {
        /// Profile name to export. When omitted, select the best automatic match.
        name: Option<String>,
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
        Commands::Daemon {
            once,
            dry_run,
            log_file,
        } => crate::daemon::run(DaemonOptions {
            once,
            dry_run,
            log_file,
        })?,
        Commands::Export { name, format } => run_export(name.as_deref(), format)?,
    }

    Ok(())
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
        let decision = decide_auto_apply(&store, &best_match, config.fallback_profile.as_deref());
        if let Some(profile_name) = decision.profile_name() {
            let profile = profile_by_name(&store, profile_name)?;
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

fn run_export(name: Option<&str>, format: ExportFormat) -> anyhow::Result<()> {
    let paths = ConfigPaths::resolve()?;
    let store = ProfileStore::load(paths.profile_store_path())?;
    let monitors = current_monitors()?;

    let profile = match name {
        Some(name) => profile_by_name(&store, name)?,
        None => {
            let config = AppConfig::load(paths.config_file_path())?;
            let best_match = best_profile_match(&store, &monitors);
            require_auto_profile(&store, &best_match, config.fallback_profile.as_deref())?
        }
    };

    let rendered = render_monitor_rules(profile, &monitors)?;
    let (path, contents) = match format {
        ExportFormat::Conf => (
            paths.generated_monitors_conf_path(),
            render_hyprland_conf(&rendered.rules)?,
        ),
        ExportFormat::Lua => (
            paths.generated_monitors_lua_path(),
            render_hyprland_lua(&rendered.rules)?,
        ),
    };

    write_generated_file(&path, &contents)?;
    println!("Exported profile `{}` to {}.", profile.name, path.display());
    Ok(())
}

fn profile_by_name<'a>(store: &'a ProfileStore, name: &str) -> anyhow::Result<&'a Profile> {
    store
        .profiles
        .iter()
        .find(|profile| profile.name == name)
        .ok_or_else(|| anyhow::anyhow!("profile `{name}` does not exist"))
}

fn require_auto_profile<'a>(
    store: &'a ProfileStore,
    best_match: &BestProfileMatch,
    fallback_profile: Option<&str>,
) -> anyhow::Result<&'a Profile> {
    match decide_auto_apply(store, best_match, fallback_profile) {
        AutoApplyDecision::Apply { profile_name, .. } => profile_by_name(store, &profile_name),
        AutoApplyDecision::NoProfiles => bail!("no profiles saved"),
        AutoApplyDecision::Ambiguous { reason } => {
            bail!("automatic apply is ambiguous: {reason}")
        }
        AutoApplyDecision::MissingFallback { profile_name } => {
            bail!("fallback_profile `{profile_name}` does not exist")
        }
        AutoApplyDecision::NotEligible { reason } => {
            bail!("no exact or high-confidence profile match: {reason}")
        }
        AutoApplyDecision::NoMatch => bail!("no useful profile match"),
    }
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
    let decision = decide_auto_apply(store, best_match, fallback_profile);
    format_auto_apply_decision(&decision, profile_label)
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
