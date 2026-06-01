<script lang="ts">
  // A document the agent materialized, streamed into an editable pane. It takes
  // the stage while it's focal; closing it returns to the conversation, and it
  // stays recallable from the History rail. (v1 edits are local — to have Myo
  // act on them, ask it in the composer.)
  import { myo } from "../lib/stage.svelte";

  const art = $derived(myo.focused);
</script>

{#if art}
  <div class="artifact">
    <header>
      <div class="meta">
        <span class="title">{art.title}</span>
        <span class="lang">{art.language}{art.version ? ` · v${art.version}` : ""}</span>
      </div>
      <button class="close" onclick={() => myo.closeArtifact()}>Close</button>
    </header>
    <textarea bind:value={art.content} spellcheck="false"></textarea>
  </div>
{/if}

<style>
  .artifact {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: #0f0f13;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.55rem 0.8rem;
    border-bottom: 1px solid #1f1f27;
  }
  .meta {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
  }
  .title {
    font-size: 0.9rem;
    font-weight: 600;
  }
  .lang {
    font-size: 0.68rem;
    color: #6f6f78;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .close {
    background: #1a1a2a;
    border: 1px solid #2c2c40;
    color: #e8e8e8;
    border-radius: 6px;
    padding: 0.35rem 0.65rem;
    font-size: 0.78rem;
    cursor: pointer;
  }
  .close:hover {
    background: #23233a;
  }
  textarea {
    flex: 1;
    resize: none;
    border: none;
    outline: none;
    background: #0f0f13;
    color: #e8e8e8;
    padding: 0.9rem;
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
    font-size: 0.82rem;
    line-height: 1.55;
  }
</style>
