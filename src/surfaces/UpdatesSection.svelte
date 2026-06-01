<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";

  // Shapes mirror the serde types in `crates/myo-self-update` (snake_case).
  interface PendingUpdate {
    version: string;
    staged_at: string;
  }
  interface UpdateStatus {
    current_version: string;
    install_kind: "raw" | "package_manager";
    enabled: boolean;
    channel: string;
    auto_apply: string;
    check_interval_hours: number;
    last_check_unix: number | null;
    pending: PendingUpdate | null;
    release_url: string;
    release_url_overridden: boolean;
  }
  type CheckOutcome =
    | { kind: "disabled" }
    | { kind: "package_manager" }
    | { kind: "up_to_date"; current: string; latest: string }
    | { kind: "staged"; version: string }
    | { kind: "policy_blocked"; current: string; latest: string; policy: string };

  let status = $state<UpdateStatus | null>(null);
  let loading = $state(true);
  let checking = $state(false);
  let togglingEnabled = $state(false);
  let outcome = $state<CheckOutcome | null>(null);
  let error = $state("");

  onMount(refresh);

  async function refresh() {
    loading = true;
    error = "";
    try {
      status = await invoke<UpdateStatus>("update_status");
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  async function checkNow() {
    checking = true;
    outcome = null;
    error = "";
    try {
      outcome = await invoke<CheckOutcome>("update_check_now");
      status = await invoke<UpdateStatus>("update_status");
    } catch (e) {
      error = String(e);
    } finally {
      checking = false;
    }
  }

  async function applyNow() {
    error = "";
    try {
      // The process restarts on success, so control usually doesn't return.
      await invoke("update_apply_now");
    } catch (e) {
      error = String(e);
    }
  }

  async function setEnabled(enabled: boolean) {
    togglingEnabled = true;
    error = "";
    try {
      status = await invoke<UpdateStatus>("update_set_enabled", { enabled });
      outcome = null; // a stale "up to date" shouldn't linger after a toggle
    } catch (e) {
      error = String(e);
    } finally {
      togglingEnabled = false;
    }
  }

  function ago(unix: number | null): string {
    if (!unix) return "never";
    const mins = Math.floor((Date.now() - unix * 1000) / 60000);
    if (mins < 1) return "just now";
    if (mins < 60) return `${mins}m ago`;
    const hours = Math.floor(mins / 60);
    if (hours < 24) return `${hours}h ago`;
    return `${Math.floor(hours / 24)}d ago`;
  }
</script>

<div class="updates">
  {#if loading}
    <div class="muted">Loading…</div>
  {:else if error && !status}
    <div class="error">{error}</div>
  {:else if status}
    <div class="row head">
      <div>
        <div class="version">myo {status.current_version}</div>
        <div class="muted">
          {status.install_kind === "package_manager" ? "package manager" : "raw binary"} ·
          {status.channel} channel
        </div>
      </div>
      <button onclick={checkNow} disabled={checking || !status.enabled}>
        {checking ? "Checking…" : "Check for updates"}
      </button>
    </div>

    <label class="row toggle">
      <div>
        <div class="label">Automatic updates</div>
        <div class="muted">
          {status.enabled
            ? `Background checks every ${status.check_interval_hours}h; applied on next launch.`
            : "Paused. You can still check manually."}
        </div>
      </div>
      <input
        type="checkbox"
        checked={status.enabled}
        disabled={togglingEnabled}
        onchange={(e) => setEnabled((e.currentTarget as HTMLInputElement).checked)}
      />
    </label>

    {#if status.install_kind === "package_manager"}
      <div class="notice">
        Installed via a package manager — self-update is intentionally disabled. Upgrade with your
        package manager.
      </div>
    {/if}

    <dl class="info">
      <div><dt>Last checked</dt><dd>{ago(status.last_check_unix)}</dd></div>
      <div><dt>Auto-apply</dt><dd><code>{status.auto_apply}</code></dd></div>
      <div><dt>Interval</dt><dd>{status.check_interval_hours}h</dd></div>
    </dl>

    <div class="feed">
      <span class="muted">Release feed{status.release_url_overridden ? " (custom)" : ""}</span>
      <code>{status.release_url}</code>
    </div>

    {#if status.pending}
      <div class="pending">
        <div><strong>Update staged: {status.pending.version}</strong></div>
        <div class="muted">Restart Myo to apply.</div>
        <button class="apply" onclick={applyNow}>Restart &amp; apply now</button>
      </div>
    {/if}

    {#if outcome}
      <div class="outcome">
        {#if outcome.kind === "disabled"}Self-update is disabled.
        {:else if outcome.kind === "package_manager"}Package-manager install — deferred to the system updater.
        {:else if outcome.kind === "up_to_date"}
          {outcome.current === outcome.latest
            ? `Already on the latest version (${outcome.latest}).`
            : `You're on ${outcome.current} — ahead of latest published (${outcome.latest}).`}
        {:else if outcome.kind === "staged"}<strong>{outcome.version}</strong> downloaded and staged. Restart to apply.
        {:else if outcome.kind === "policy_blocked"}{outcome.latest} is available but auto_apply="{outcome.policy}" doesn't permit this jump from {outcome.current}.
        {/if}
      </div>
    {/if}

    {#if error}<div class="error">{error}</div>{/if}
  {/if}
</div>

<style>
  .updates {
    display: flex;
    flex-direction: column;
    gap: 0.85rem;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
  }
  .toggle {
    background: #131318;
    border: 1px solid #1f1f27;
    border-radius: 8px;
    padding: 0.65rem 0.8rem;
    cursor: pointer;
  }
  .version {
    font-size: 1rem;
    font-weight: 600;
  }
  .label {
    font-weight: 500;
  }
  .muted {
    color: #7d7d87;
    font-size: 0.78rem;
  }
  button {
    background: #1a1a2a;
    border: 1px solid #2c2c40;
    color: #e8e8e8;
    border-radius: 6px;
    padding: 0.45rem 0.8rem;
    font-size: 0.8rem;
    cursor: pointer;
  }
  button:hover:not(:disabled) {
    background: #23233a;
  }
  button:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .notice {
    background: #2a220e;
    border: 1px solid #3a2e0e;
    color: #d6b25a;
    border-radius: 7px;
    padding: 0.55rem 0.75rem;
    font-size: 0.78rem;
  }
  .info {
    margin: 0;
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 0.6rem;
    background: #131318;
    border: 1px solid #1f1f27;
    border-radius: 8px;
    padding: 0.7rem;
  }
  .info > div {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
  }
  dt {
    font-size: 0.66rem;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    color: #6f6f78;
  }
  dd {
    margin: 0;
    font-size: 0.82rem;
  }
  code {
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
    font-size: 0.74rem;
    color: #9bbfe0;
    word-break: break-all;
  }
  .feed {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    background: #131318;
    border: 1px solid #1f1f27;
    border-radius: 8px;
    padding: 0.65rem 0.8rem;
  }
  .pending {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    background: #14221a;
    border: 1px solid #1e3325;
    border-radius: 8px;
    padding: 0.7rem 0.8rem;
  }
  .pending strong {
    color: #7dd39b;
  }
  .apply {
    align-self: flex-start;
    background: #1f3a26;
    border-color: #2c5135;
    color: #cfeacf;
  }
  .apply:hover {
    background: #28492f;
  }
  .outcome {
    background: #131820;
    border: 1px solid #1e2530;
    border-radius: 8px;
    padding: 0.6rem 0.8rem;
    font-size: 0.82rem;
    color: #aab;
  }
  .outcome strong {
    color: #e8e8e8;
  }
  .error {
    color: #e07a7a;
    font-size: 0.82rem;
  }
</style>
