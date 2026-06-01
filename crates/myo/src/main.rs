//! `myo` — the entrypoint binary.
//!
//! Today this is the headless seed of the Myo orchestrator described in
//! `docs/PLAN.md`: it self-updates and exposes the update CLI. As the Tauri
//! shell, ASR, and brain client land (PLAN steps 2–7), they grow into this
//! same crate — exactly how MyOwnLLM's single binary is both a CLI and a GUI.
//!
//! The one behavior that's load-bearing from day one: every launch first
//! applies any update a previous run staged, so an upgrade lands the next time
//! the user opens Myo with zero interaction.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    // First thing, before anything else: apply any staged self-update. Errors
    // are logged and swallowed inside — an update hiccup must never stop Myo
    // from starting.
    myo_self_update::apply_pending_if_any();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Vec<String>) -> anyhow::Result<()> {
    match args.first().map(|s| s.as_str()) {
        // One-shot upgrade + the status/check/apply/enable/disable subcommands.
        Some("update") => myo_self_update::cmd_update(&args[1..]).await,

        // Foreground background-checker (for a service unit / launch agent).
        Some("watch") => {
            myo_self_update::watcher::watch_forever().await;
            Ok(())
        }

        Some("--version") | Some("-V") | Some("version") => {
            println!("myo {}", myo_self_update::current_version());
            Ok(())
        }

        Some("help") | Some("--help") | Some("-h") | None => {
            print_help();
            Ok(())
        }

        Some(unknown) => Err(anyhow::anyhow!(
            "unknown command: {unknown}\nRun `myo help` for usage."
        )),
    }
}

fn print_help() {
    println!(
        r#"myo — local voice-first AI companion

USAGE:
  myo [command]
  myo --version

COMMANDS:
  update        Update to the latest release (one shot: check + download + verify + apply)
                Subcommands: status | check | apply | enable | disable
  watch         Run the background update watcher in the foreground
  help          Show this help

Every launch first applies any update staged by a previous run, so upgrades are
hands-free. Self-update honors ~/.myo/config.json (auto_update.*) and is skipped
for package-manager installs (Homebrew/apt/rpm/MSI/Chocolatey/Scoop) and when
MYO_AUTOUPDATE=0."#
    );
}
