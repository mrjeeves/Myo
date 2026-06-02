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

mod core_api;
mod engine_update;
mod events;
mod state;
mod supervisor;
mod update_commands;
#[cfg(windows)]
mod windows;

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

/// GUI mode: open the desktop window, register the Core API + update commands,
/// bring the engines up, and start the background watcher — all on Tauri's
/// async runtime.
fn run_gui() {
    // Windows/dev: if a parent like `cargo` (under `just dev`) dies, come down
    // with it so the engines Myo spawned don't orphan — a Tauri GUI app detaches
    // from the terminal, so a shell Ctrl-C never reaches it. No-op for
    // Explorer-launched / installed runs.
    #[cfg(windows)]
    windows::install_parent_watchdog();

    // A bare `myo` on a headless box would otherwise exit silently when the
    // webview can't find a display — point the user at the CLI instead.
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("myo: no DISPLAY or WAYLAND_DISPLAY — can't open the desktop window.");
        eprintln!("On a headless box, try: myo update | myo watch | myo --version");
        std::process::exit(1);
    }

    // Build the shared state up front: load the persisted brain token (injected
    // into Odysseus's env when the supervisor spawns it — persisted, not minted
    // per-launch, so Myo can reuse / re-attach to a brain a previous run left
    // running without a token mismatch 403'ing every call), load persisted
    // settings, and create the loopback brain client.
    let token = myo_core::supervisor::persistent_token();
    let settings = myo_core::ShellSettings::load().unwrap_or_default();
    let brain = myo_core::BrainClient::new(myo_core::BrainConfig::new(
        myo_core::supervisor::odysseus_base_url(),
        token.clone(),
        "myo",
    ))
    .expect("failed to build the brain client");
    // The ears: an HTTP client for Myo's *own* engine on its private port
    // (`:11473`, not the shared `:1473`), so transcription never attaches to a
    // user's separately-run / stale MyOwnLLM.
    let asr = myo_core::AsrClient::new(myo_core::supervisor::myownllm_base_url())
        .expect("failed to build the ASR client");
    // The voice: the same private engine's `/v1/audio/speech` — a
    // hardware-tiered synthesized voice, with WebSpeech as the fallback.
    let tts = myo_core::TtsClient::new(myo_core::supervisor::myownllm_base_url())
        .expect("failed to build the TTS client");
    // The native brain: streams chat straight from MyOwnLLM (the same private
    // engine the ears use), so a conversation needs no Odysseus brain.
    let llm = myo_core::LlmClient::new(myo_core::supervisor::myownllm_base_url())
        .expect("failed to build the LLM client");
    let app_state =
        std::sync::Arc::new(state::MyoState::new(token, brain, asr, tts, llm, settings));
    // A clone for the exit hook below (the setup closure moves the other one).
    let exit_state = app_state.clone();

    tauri::Builder::default()
        .manage(app_state.clone())
        .invoke_handler(tauri::generate_handler![
            update_status,
            update_check_now,
            update_apply_now,
            update_set_enabled,
            update_leftovers_list,
            update_leftovers_clear,
            core_api::myo_engines_status,
            core_api::myo_engines_ensure_ready,
            core_api::myo_asr_stream_url,
            core_api::myo_converse_say,
            core_api::myo_converse_cancel,
            core_api::myo_converse_feed_wav,
            core_api::myo_converse_feed_audio,
            core_api::myo_converse_incognito,
            core_api::myo_capabilities_get,
            core_api::myo_capabilities_set,
            core_api::myo_memory_list,
            core_api::myo_memory_forget,
            core_api::myo_settings_get,
            core_api::myo_persona_get,
            core_api::myo_persona_set,
            core_api::myo_tts_speak,
        ])
        .setup(move |app| {
            // Keep checking for updates in the background; the swap is applied
            // next launch. Use Tauri's runtime — there's no `#[tokio::main]`.
            tauri::async_runtime::spawn(myo_self_update::watcher::watch_forever());
            // Bring the brain + model engine up and wire them together.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(supervisor::ensure_ready(app_handle, app_state.clone()));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building myo")
        .run(move |_app_handle, event| {
            // When Myo closes, close the engines it started — so the whole stack
            // comes down with it (and each engine, like MyOwnLLM, closes its own
            // children in turn). Tauri may `process::exit` on close without
            // unwinding, so kill-on-Drop alone wouldn't fire; do it here.
            if let tauri::RunEvent::Exit = event {
                exit_state.shutdown();
            }
        });
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
