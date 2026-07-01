use anyhow::bail;
use clap::{Parser, Subcommand, ValueEnum};

use crate::hyprland::hyprctl::HyprctlClient;
use crate::hyprland::monitor::MonitorState;

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
        Commands::List => not_implemented("list")?,
        Commands::Save { .. } => not_implemented("save")?,
        Commands::Apply { .. } => not_implemented("apply")?,
        Commands::Daemon => not_implemented("daemon")?,
        Commands::Export { .. } => not_implemented("export")?,
    }

    Ok(())
}

fn not_implemented(command: &str) -> anyhow::Result<()> {
    bail!("`{command}` is not implemented yet")
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
