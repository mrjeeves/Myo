//! `myo` — entrypoint: headless CLI + desktop shell (Tauri).
//!
//! Mirrors MyOwnLLM's single-binary model. With arguments it runs as a CLI
//! (`myo update …`, `myo watch`); with none it opens the desktop window. Either
//! way it FIRST applies any self-update staged by a previous run, so upgrades
//! land hands-free.
//!
//! This is the seed of the Myo orchestrator from `docs/PLAN.md`: the voice
//! spine, ASR, and brain client (PLAN steps 2–7) grow into this same crate. For
//! now the desktop window hosts the auto-update Settings panel — proving the
//! engine ↔ Tauri command ↔ Svelte surface path end to end.

// NOTE: we stay on the console subsystem on Windows so `myo update` prints to
// the terminal. The GUI-subsystem + parent-console-attach polish (so the
// window launches without a console flash) is a later Windows task.

mod update_commands;

use update_commands::{
    update_apply_now, update_check_now, update_leftovers_clear, update_leftovers_list,
    update_set_enabled, update_status,
};

fn main() {
    // Before anything — ports, windows, work — apply any update a previous run
    // staged. Errors are logged and swallowed inside; never fatal.
    myo_self_update::apply_pending_if_any();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        run_gui();
    } else {
        run_cli(args);
    }
}

/// CLI mode: spin up a Tokio runtime, dispatch the subcommand, exit with its
/// status. (Bare `fn main` — Tauri owns the runtime in GUI mode.)
fn run_cli(args: Vec<String>) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let code = rt.block_on(async {
        match dispatch(args).await {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        }
    });
    std::process::exit(code);
}

async fn dispatch(args: Vec<String>) -> anyhow::Result<()> {
    match args.first().map(|s| s.as_str()) {
        Some("update") => myo_self_update::cmd_update(&args[1..]).await,
        Some("watch") => {
            myo_self_update::watcher::watch_forever().await;
            Ok(())
        }
        Some("--version") | Some("-V") | Some("version") => {
            println!("myo {}", myo_self_update::current_version());
            Ok(())
        }
        Some("help") | Some("--help") | Some("-h") => {
            print_help();
            Ok(())
        }
        Some(unknown) => Err(anyhow::anyhow!(
            "unknown command: {unknown}\nRun `myo help` for usage."
        )),
        None => {
            print_help();
            Ok(())
        }
    }
}

/// GUI mode: open the desktop window, register the update commands, and start
/// the background watcher on Tauri's async runtime.
fn run_gui() {
    // A bare `myo` on a headless box would otherwise exit silently when the
    // webview can't find a display — point the user at the CLI instead.
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("myo: no DISPLAY or WAYLAND_DISPLAY — can't open the desktop window.");
        eprintln!("On a headless box, try: myo update | myo watch | myo --version");
        std::process::exit(1);
    }

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            update_status,
            update_check_now,
            update_apply_now,
            update_set_enabled,
            update_leftovers_list,
            update_leftovers_clear,
        ])
        .setup(|_app| {
            // Keep checking in the background; the swap is applied next launch.
            // Use Tauri's runtime — there's no `#[tokio::main]` here.
            tauri::async_runtime::spawn(myo_self_update::watcher::watch_forever());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running myo");
}

fn print_help() {
    println!(
        r#"myo — local voice-first AI companion

USAGE:
  myo                 Open the desktop window
  myo [command]
  myo --version

COMMANDS:
  update        Update to the latest release (check + download + verify + apply)
                Subcommands: status | check | apply | enable | disable
  watch         Run the background update watcher in the foreground
  help          Show this help

Every launch first applies any update staged by a previous run, so upgrades are
hands-free. Self-update honors ~/.myo/config.json (auto_update.*) and is skipped
for package-manager installs and when MYO_AUTOUPDATE=0."#
    );
}
