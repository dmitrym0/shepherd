mod detect;
mod osc;
mod protocol;
mod server;
mod state;
mod status;
mod supervise;
mod wrapper;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "shep", version, about = "Run AI agents in your terminal; monitor their status anywhere")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run an agent under supervision in this terminal.
    Run {
        /// Display name for this agent in monitors.
        #[arg(long)]
        name: Option<String>,
        /// The agent command to run, e.g. `claude --model opus`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// Run the aggregation server in the foreground. `shep run`
    /// auto-starts one when none is running.
    Serve,
    /// Show supervised agents.
    Status {
        /// Keep watching; re-render on every change.
        #[arg(long, short)]
        watch: bool,
    },
    /// Install the Claude Code SessionStart hook so Claude sessions report
    /// their resumable session id.
    InstallClaudeHook,
    /// Hook entry point invoked by Claude Code (installed by
    /// install-claude-hook).
    #[command(hide = true)]
    ClaudeHook { action: String },
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Run { name, command } => match wrapper::run(name, command) {
            Ok(code) => code,
            Err(err) => {
                eprintln!("shepherd: {err}");
                1
            }
        },
        Command::Serve => {
            let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should start");
            match runtime.block_on(server::serve()) {
                Ok(()) => 0,
                Err(err) => {
                    eprintln!("shepherd: {err}");
                    1
                }
            }
        }
        Command::Status { watch } => match status::status(watch) {
            Ok(()) => 0,
            Err(err) => {
                eprintln!("shepherd: {err}");
                1
            }
        },
        Command::InstallClaudeHook => match status::install_claude_hook() {
            Ok(path) => {
                println!("installed SessionStart hook into {}", path.display());
                0
            }
            Err(err) => {
                eprintln!("shepherd: {err}");
                1
            }
        },
        // Hooks must never break the agent that invoked them: swallow all
        // errors and exit 0.
        Command::ClaudeHook { action } => {
            let _ = status::claude_hook(&action);
            0
        }
    };
    std::process::exit(code);
}
