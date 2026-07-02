use std::env;
use std::fs;

use anyhow::bail;
use clap::{Parser, Subcommand, ValueEnum};

use crate::config::ConfigPaths;
use crate::hyprland::hyprctl::parse_monitors_output;
use crate::hyprland::hyprctl::HyprctlClient;
use crate::hyprland::monitor::MonitorState;
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
            let client = HyprctlClient;
            let monitors = client.monitors_all()?;
            println!("{}", format_doctor(&monitors));
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
        Commands::Apply { .. } => not_implemented("apply")?,
        Commands::Daemon => not_implemented("daemon")?,
        Commands::Export { .. } => not_implemented("export")?,
    }

    Ok(())
}

fn not_implemented(command: &str) -> anyhow::Result<()> {
    bail!("`{command}` is not implemented yet")
}

fn current_monitors() -> anyhow::Result<Vec<MonitorState>> {
    if let Some(path) = env::var_os(MONITORS_JSON_ENV) {
        let contents = fs::read_to_string(&path)?;
        return parse_monitors_output(&contents);
    }

    let client = HyprctlClient;
    client.monitors_all()
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
