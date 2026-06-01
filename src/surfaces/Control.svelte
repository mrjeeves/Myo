<script lang="ts">
  // The control surface (tier-a): four capability toggles that drive Odysseus's
  // own permission knobs, plus the privacy / incognito switch. Default posture
  // is Web on, everything else off — the agent can look things up, but touching
  // files, running code, or reaching out to people is opt-in.
  import { myo } from "../lib/stage.svelte";
  import type { Capabilities } from "../lib/core-api";

  const rows: { key: keyof Capabilities; label: string; hint: string }[] = [
    { key: "web", label: "Web", hint: "Search and research the web" },
    { key: "files", label: "Files", hint: "Read and write files and documents" },
    { key: "code", label: "Code", hint: "Run shell and Python" },
    { key: "reach_out", label: "Reach-out", hint: "Email, calendar, contacts" },
  ];
</script>

<div class="control">
  <p class="lead">What Myo is allowed to do</p>
  {#each rows as row (row.key)}
    <label class="toggle">
      <div>
        <div class="label">{row.label}</div>
        <div class="hint">{row.hint}</div>
      </div>
      <input
        type="checkbox"
        checked={myo.capabilities[row.key]}
        onchange={(e) => myo.setCapability(row.key, e.currentTarget.checked)}
      />
    </label>
  {/each}

  <label class="toggle privacy">
    <div>
      <div class="label">Incognito</div>
      <div class="hint">Pause memory — this conversation isn't remembered</div>
    </div>
    <input
      type="checkbox"
      checked={myo.incognito}
      onchange={(e) => myo.setIncognito(e.currentTarget.checked)}
    />
  </label>
</div>

<style>
  .control {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .lead {
    margin: 0 0 0.2rem;
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: #6f6f78;
  }
  .toggle {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    background: #131318;
    border: 1px solid #1f1f27;
    border-radius: 8px;
    padding: 0.55rem 0.75rem;
    cursor: pointer;
  }
  .toggle.privacy {
    margin-top: 0.4rem;
    border-color: #2e2640;
    background: #16131c;
  }
  .label {
    font-weight: 500;
    font-size: 0.86rem;
  }
  .hint {
    color: #7d7d87;
    font-size: 0.72rem;
  }
  input {
    width: 18px;
    height: 18px;
    accent-color: #5b9bd5;
  }
</style>
