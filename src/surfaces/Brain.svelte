<script lang="ts">
  // The Brain surface: view and edit Myo's persona — the system prompt that
  // opens every conversation. An empty value (or "Reset to default") restores
  // the built-in MYO_PERSONA. Persisted to ~/.myo via the Core API.
  import { onMount } from "svelte";
  import { myo } from "../lib/stage.svelte";

  let draft = $state("");
  let loaded = $state(false);

  onMount(async () => {
    await myo.loadPersona();
    draft = myo.persona?.effective ?? "";
    loaded = true;
  });

  const dirty = $derived(
    loaded && draft.trim() !== (myo.persona?.effective ?? "").trim(),
  );

  async function save() {
    await myo.savePersona(draft);
    draft = myo.persona?.effective ?? "";
  }

  async function reset() {
    await myo.resetPersona();
    draft = myo.persona?.effective ?? "";
  }
</script>

<div class="brain">
  <p class="lead">Myo's persona</p>
  <p class="hint">
    The system prompt that opens every conversation — Myo's character, tone, and
    standing instructions. Keep the default, or make it yours.
  </p>

  <textarea
    bind:value={draft}
    rows="12"
    spellcheck="false"
    placeholder={loaded ? "" : "Loading…"}
    disabled={!loaded}
  ></textarea>

  <div class="row">
    <span class="tag" class:custom={myo.persona?.custom}>
      {myo.persona?.custom ? "Custom" : "Default"}
    </span>
    <div class="actions">
      <button
        class="ghost"
        onclick={reset}
        disabled={!loaded || !myo.persona?.custom}
        title="Restore the built-in persona"
      >
        Reset to default
      </button>
      <button class="primary" onclick={save} disabled={!dirty}>Save</button>
    </div>
  </div>
</div>

<style>
  .brain {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }
  .lead {
    margin: 0;
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: #6f6f78;
  }
  .hint {
    margin: 0;
    color: #7d7d87;
    font-size: 0.74rem;
    line-height: 1.45;
  }
  textarea {
    width: 100%;
    box-sizing: border-box;
    resize: vertical;
    background: #131318;
    border: 1px solid #1f1f27;
    border-radius: 8px;
    color: #e3e3ea;
    padding: 0.6rem 0.7rem;
    font: 0.82rem/1.5 ui-monospace, SFMono-Regular, Menlo, monospace;
  }
  textarea:focus {
    outline: none;
    border-color: #2c5168;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
  }
  .tag {
    font-size: 0.68rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: #6f6f78;
    border: 1px solid #23232c;
    border-radius: 5px;
    padding: 0.12rem 0.4rem;
  }
  .tag.custom {
    color: #7dd39b;
    border-color: #265038;
    background: #11231a;
  }
  .actions {
    display: flex;
    gap: 0.4rem;
  }
  button {
    border-radius: 6px;
    padding: 0.35rem 0.7rem;
    font-size: 0.8rem;
    cursor: pointer;
    border: 1px solid #23232c;
    background: #16161c;
    color: #cfcfd6;
  }
  button:hover:not(:disabled) {
    background: #1c1c24;
  }
  button:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .primary {
    border-color: #2c5168;
    background: #14222d;
    color: #cfe6f2;
  }
  .ghost {
    background: transparent;
  }
</style>
