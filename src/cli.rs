use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::bail;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};

use crate::config::{write_generated_file, AppConfig, ConfigPaths};
use crate::daemon::{
    decide_auto_apply, format_auto_apply_decision, AutoApplyDecision, DaemonOptions,
};
use crate::hyprland::hyprctl::current_monitors;
use crate::hyprland::monitor::MonitorState;
use crate::profile::apply::{apply_plan, ensure_plan_safe_to_apply, plan_apply, ApplyPlan};
use crate::profile::r#match::{best_profile_match, BestProfileMatch};
use crate::profile::render::{format_hyprctl_batch_command, render_hyprland_lua};
use crate::profile::store::Profile;
use crate::profile::store::ProfileStore;
use crate::systemd::{install_user_service, SystemdInstallOptions};

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
        /// Profile name. A collision-safe name is generated when omitted.
        name: Option<String>,
        /// Replace an existing profile with the same name.
        #[arg(long)]
        replace: bool,
    },
    /// Rename a saved profile.
    Rename {
        /// Existing profile name.
        old: String,
        /// New profile name.
        new: String,
    },
    /// Delete a saved profile.
    Delete {
        /// Profile name to delete.
        name: String,
        /// Delete without an interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Copy a saved profile to a new name.
    Copy {
        /// Existing profile name.
        source: String,
        /// New profile name.
        destination: String,
        /// Replace an existing destination profile.
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
    /// Open the terminal profile editor.
    Tui,
    /// Install the HyprDisJust daemon as a systemd user service.
    InstallSystemdUser {
        /// Enable the user service after writing it.
        #[arg(long)]
        enable: bool,
        /// Start the user service after writing it.
        #[arg(long)]
        start: bool,
        /// Print the service that would be installed without writing files.
        #[arg(long)]
        dry_run: bool,
    },
    /// Print shell completions.
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ExportFormat {
    Lua,
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Doctor => {
            let paths = ConfigPaths::resolve()?;
            let report = crate::doctor::build_doctor_report(&paths);
            println!("{}", format_doctor_report(&report));
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
        Commands::Rename { old, new } => run_rename(&old, &new)?,
        Commands::Delete { name, yes } => run_delete(&name, yes)?,
        Commands::Copy {
            source,
            destination,
            replace,
        } => run_copy(&source, &destination, replace)?,
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
        Commands::Tui => run_tui()?,
        Commands::InstallSystemdUser {
            enable,
            start,
            dry_run,
        } => run_install_systemd_user(enable, start, dry_run)?,
        Commands::Completions { shell } => print!("{}", render_completions(shell)?),
    }

    Ok(())
}

fn run_rename(old: &str, new: &str) -> anyhow::Result<()> {
    let paths = ConfigPaths::resolve()?;
    let mut store = ProfileStore::load(paths.profile_store_path())?;
    store.rename_profile(old, new)?;
    store.save_atomic(paths.profile_store_path())?;
    println!("Renamed profile `{old}` to `{new}`.");
    Ok(())
}

fn run_delete(name: &str, yes: bool) -> anyhow::Result<()> {
    if !yes && !confirm_delete(name)? {
        println!("Delete cancelled.");
        return Ok(());
    }

    let paths = ConfigPaths::resolve()?;
    let mut store = ProfileStore::load(paths.profile_store_path())?;
    store.delete_profile(name)?;
    store.save_atomic(paths.profile_store_path())?;
    println!("Deleted profile `{name}`.");
    Ok(())
}

fn run_copy(source: &str, destination: &str, replace: bool) -> anyhow::Result<()> {
    let paths = ConfigPaths::resolve()?;
    let mut store = ProfileStore::load(paths.profile_store_path())?;
    store.copy_profile(source, destination, replace)?;
    store.save_atomic(paths.profile_store_path())?;
    println!("Copied profile `{source}` to `{destination}`.");
    Ok(())
}

fn confirm_delete(name: &str) -> anyhow::Result<bool> {
    if !io::stdin().is_terminal() {
        bail!("delete requires --yes when stdin is not an interactive terminal");
    }

    print!("Delete profile `{name}`? type `yes` to confirm: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(answer.trim() == "yes")
}

fn run_tui() -> anyhow::Result<()> {
    let paths = ConfigPaths::resolve()?;
    let store = ProfileStore::load(paths.profile_store_path())?;
    let monitors = current_monitors()?;
    let config = AppConfig::load(paths.config_file_path())?;

    if std::io::stdout().is_terminal() {
        let app = crate::tui::TuiApp::new(store, paths, config, monitors)?;
        crate::tui::run(app)
    } else {
        let model =
            crate::tui::initial_model(&store, &monitors, config.fallback_profile.as_deref())?;
        println!("{}", crate::tui::format_snapshot(&model));
        Ok(())
    }
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
        let plan = plan_apply(profile, &monitors)?;
        apply_or_print(&plan, &monitors, dry_run)?;
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
            let plan = plan_apply(profile, &monitors)?;
            output.push_str("\n\n");
            output.push_str(&format_apply_commands(&plan));
        } else {
            output.push_str("\n\nCommands: none");
        }
        println!("{output}");
        return Ok(());
    }

    let profile = require_auto_profile(&store, &best_match, config.fallback_profile.as_deref())?;
    let plan = plan_apply(profile, &monitors)?;
    apply_or_print(&plan, &monitors, dry_run)
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

    let plan = plan_apply(profile, &monitors)?;
    ensure_plan_safe_to_apply(&plan)?;
    print_apply_warnings(&plan);
    let path = match format {
        ExportFormat::Lua => paths.generated_monitors_lua_path(),
    };
    let contents = render_hyprland_lua(&plan.rules)?;

    write_generated_file(&path, &contents)?;
    println!("Exported profile `{}` to {}.", profile.name, path.display());
    Ok(())
}

fn run_install_systemd_user(enable: bool, start: bool, dry_run: bool) -> anyhow::Result<()> {
    let result = install_user_service(&SystemdInstallOptions {
        enable,
        start,
        dry_run,
    })?;

    if dry_run {
        println!(
            "Would write systemd user service to {}:\n{}",
            result.service_path.display(),
            result.service_contents
        );
        return Ok(());
    }

    println!(
        "Installed systemd user service at {}.",
        result.service_path.display()
    );
    if result.enabled {
        println!("Enabled hyprdisjust.service.");
    }
    if result.started {
        println!("Started hyprdisjust.service.");
    }
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
    plan: &ApplyPlan,
    previous_monitors: &[MonitorState],
    dry_run: bool,
) -> anyhow::Result<()> {
    if dry_run {
        println!("{}", format_apply_commands(plan));
        return Ok(());
    }

    print_apply_warnings(plan);

    if plan.is_noop {
        println!(
            "No changes: profile `{}` is already active.",
            plan.profile_name
        );
        return Ok(());
    }

    if let Err(error) = apply_plan(plan) {
        bail!(
            "{error:#}\nPrevious layout:\n{}",
            format_doctor(previous_monitors)
        );
    }

    println!(
        "Applied profile `{}` with {} monitor rule{}.",
        plan.profile_name,
        plan.rules.len(),
        if plan.rules.len() == 1 { "" } else { "s" }
    );
    Ok(())
}

fn format_apply_commands(plan: &ApplyPlan) -> String {
    let safety_error = ensure_plan_safe_to_apply(plan).err();
    let mut output = format!(
        "Profile: {}\nOperation: {}\nGenerated command:\n{}",
        plan.profile_name,
        if let Some(error) = &safety_error {
            format!("refused: {error}")
        } else if plan.is_noop {
            "no changes; the profile is already active".to_owned()
        } else {
            "apply the generated monitor batch".to_owned()
        },
        format_hyprctl_batch_command(&plan.batch)
    );
    if !plan.warnings.is_empty() {
        output.push('\n');
        output.push_str(&format_apply_warnings(plan));
    }
    output
}

fn print_apply_warnings(plan: &ApplyPlan) {
    if !plan.warnings.is_empty() {
        println!("{}", format_apply_warnings(plan));
    }
}

fn format_apply_warnings(plan: &ApplyPlan) -> String {
    let mut output = "Warnings:".to_owned();
    for warning in &plan.warnings {
        output.push_str("\n- ");
        output.push_str(&warning.message());
    }
    output
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

pub fn format_doctor_report(report: &crate::doctor::DoctorReport) -> String {
    let mut output = "HyprDisJust doctor".to_owned();
    output.push_str("\n\nChecks:");
    for check in &report.checks {
        output.push_str(&format!(
            "\n[{}] {}: {}",
            format_doctor_severity(check.severity),
            check.label,
            check.message
        ));
    }

    output.push_str("\n\n");
    if report.monitors.is_empty() {
        output.push_str("Hyprland: not detected\nMonitors: 0");
    } else {
        output.push_str(&format_doctor(&report.monitors));
    }

    if let Some(summary) = &report.best_profile_summary {
        output.push_str("\n\n");
        output.push_str(summary);
    }

    output
}

fn format_doctor_severity(severity: crate::doctor::DoctorSeverity) -> &'static str {
    match severity {
        crate::doctor::DoctorSeverity::Ok => "ok",
        crate::doctor::DoctorSeverity::Info => "info",
        crate::doctor::DoctorSeverity::Warning => "warning",
        crate::doctor::DoctorSeverity::Error => "error",
    }
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

fn render_completions(shell: Shell) -> anyhow::Result<String> {
    let mut command = Cli::command();
    let mut output = Vec::new();
    generate(shell, &mut command, "hyprdisjust", &mut output);
    Ok(String::from_utf8(output)?)
}
