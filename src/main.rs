mod detect;
mod install;
mod osc;
mod protocol;
mod server;
mod state;
mod status;
mod store;
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
        /// Filter by metadata: `key=value` (exact key) or a bare term
        /// (substring over all keys and values).
        filter: Option<String>,
        /// Keep watching; re-render on every change.
        #[arg(long, short)]
        watch: bool,
    },
    /// Set session metadata: `shep meta [agent] key=value...`. An empty
    /// value (`key=`) removes the key; no entries prints current metadata.
    /// Without an agent, targets the supervised session this command runs in.
    Meta {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Install an agent integration so sessions report their own state and
    /// resumable session id, e.g. `shep install claude opencode`.
    Install {
        /// Agents to install: claude, opencode.
        #[arg(required = true)]
        agents: Vec<String>,
    },
    /// Deprecated alias for `shep install claude`.
    #[command(hide = true)]
    InstallClaudeHook,
    /// Hook entry point invoked by Claude Code (installed by
    /// install-claude-hook).
    #[command(hide = true)]
    ClaudeHook { action: String },
}

/// Install each named agent, reporting every path written. One bad agent does
/// not stop the others; the exit code reflects whether anything failed.
fn install_agents(agents: &[String]) -> i32 {
    let mut code = 0;
    for agent in agents {
        match install::install(agent) {
            Ok(messages) => {
                for message in messages {
                    println!("{message}");
                }
            }
            Err(err) => {
                eprintln!("shepherd: {agent}: {err}");
                code = 1;
            }
        }
    }
    code
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
        Command::Status { filter, watch } => match status::status(watch, filter) {
            Ok(()) => 0,
            Err(err) => {
                eprintln!("shepherd: {err}");
                1
            }
        },
        Command::Meta { args } => match status::meta(args) {
            Ok(()) => 0,
            Err(err) => {
                eprintln!("shepherd: {err}");
                1
            }
        },
        Command::Install { agents } => install_agents(&agents),
        Command::InstallClaudeHook => install_agents(&["claude".to_string()]),
        // Hooks must never break the agent that invoked them: swallow all
        // errors and exit 0.
        Command::ClaudeHook { action } => {
            let _ = status::claude_hook(&action);
            0
        }
    };
    std::process::exit(code);
}
