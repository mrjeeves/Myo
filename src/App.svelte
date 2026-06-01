<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { myo } from "./lib/stage.svelte";
  import { micAvailable } from "./lib/audio-io";
  import Presence from "./surfaces/Presence.svelte";
  import Conversation from "./surfaces/Conversation.svelte";
  import DocumentArtifact from "./surfaces/DocumentArtifact.svelte";
  import Composer from "./surfaces/Composer.svelte";
  import Control from "./surfaces/Control.svelte";
  import Memory from "./surfaces/Memory.svelte";
  import UpdatesSection from "./surfaces/UpdatesSection.svelte";

  onMount(async () => {
    await myo.init();
    myo.setMicReady(await micAvailable());
  });
  onDestroy(() => myo.dispose());

  const panelTitles: Record<string, string> = {
    control: "What Myo can do",
    memory: "Memory",
    settings: "Updates",
  };
</script>

<div class="app">
  <header class="bar">
    <div class="left">
      <h1>Myo</h1>
      <Presence />
    </div>
    <div class="right">
      <span class="dot" class:on={myo.engines.odysseus} title="Brain — Odysseus">brain</span>
      <span class="dot" class:on={myo.engines.myownllm} title="Model engine — MyOwnLLM">model</span>
      <button onclick={() => myo.showPanel("control")} title="Controls" aria-label="Controls">⚙</button>
      <button onclick={() => myo.showPanel("memory")} title="Memory" aria-label="Memory">🧠</button>
      <button onclick={() => myo.showPanel("settings")} title="Updates" aria-label="Updates">↻</button>
    </div>
  </header>

  {#if myo.artifacts.length}
    <div class="rail" title="Recall a document">
      {#each myo.artifacts as a, i (i)}
        <button class="chip" class:active={myo.focusedArtifact === i} onclick={() => myo.recallArtifact(i)}>
          {a.title}
        </button>
      {/each}
      {#if myo.focusedArtifact !== null}
        <button class="chip ghost" onclick={() => myo.closeArtifact()}>← conversation</button>
      {/if}
    </div>
  {/if}

  <main>
    {#if myo.focused}
      <DocumentArtifact />
    {:else}
      <Conversation />
    {/if}
  </main>

  <Composer />

  {#if myo.openPanel}
    <button class="backdrop" aria-label="Close panel" onclick={() => myo.showPanel(null)}></button>
    <aside class="panel">
      <header>
        <h2>{panelTitles[myo.openPanel] ?? myo.openPanel}</h2>
        <button class="x" onclick={() => myo.showPanel(null)} aria-label="Close">✕</button>
      </header>
      <div class="panel-body">
        {#if myo.openPanel === "control"}
          <Control />
        {:else if myo.openPanel === "memory"}
          <Memory />
        {:else if myo.openPanel === "settings"}
          <UpdatesSection />
        {/if}
      </div>
    </aside>
  {/if}
</div>

<style>
  :global(:root) {
    color-scheme: dark;
  }
  :global(body) {
    margin: 0;
    background: #0d0d10;
    color: #e8e8e8;
    font: 14px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif;
  }
  .app {
    display: flex;
    flex-direction: column;
    height: 100vh;
    box-sizing: border-box;
  }
  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.55rem 0.8rem;
    border-bottom: 1px solid #1f1f27;
    flex: none;
  }
  .left {
    display: flex;
    align-items: baseline;
    gap: 0.7rem;
  }
  h1 {
    margin: 0;
    font-size: 1.05rem;
    letter-spacing: 0.01em;
  }
  .right {
    display: flex;
    align-items: center;
    gap: 0.3rem;
  }
  .dot {
    font-size: 0.6rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: #6f6f78;
    padding: 0.15rem 0.35rem;
    border: 1px solid #23232c;
    border-radius: 5px;
  }
  .dot.on {
    color: #7dd39b;
    border-color: #265038;
    background: #11231a;
  }
  .right button {
    background: transparent;
    border: 1px solid transparent;
    color: #cfcfd6;
    border-radius: 6px;
    padding: 0.2rem 0.35rem;
    font-size: 0.95rem;
    cursor: pointer;
    line-height: 1;
  }
  .right button:hover {
    background: #1a1a22;
    border-color: #23232c;
  }
  .rail {
    display: flex;
    gap: 0.35rem;
    padding: 0.4rem 0.6rem;
    overflow-x: auto;
    border-bottom: 1px solid #1f1f27;
    flex: none;
  }
  .chip {
    flex: none;
    background: #131318;
    border: 1px solid #23232c;
    color: #b9c2cc;
    border-radius: 999px;
    padding: 0.25rem 0.7rem;
    font-size: 0.74rem;
    cursor: pointer;
    white-space: nowrap;
  }
  .chip.active {
    border-color: #2c5168;
    background: #14222d;
    color: #cfe6f2;
  }
  .chip.ghost {
    color: #7d7d87;
  }
  main {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    border: none;
    cursor: default;
  }
  .panel {
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    width: min(86vw, 360px);
    background: #0f0f13;
    border-left: 1px solid #23232c;
    display: flex;
    flex-direction: column;
    box-shadow: -8px 0 24px rgba(0, 0, 0, 0.4);
  }
  .panel header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.7rem 0.9rem;
    border-bottom: 1px solid #1f1f27;
  }
  .panel h2 {
    margin: 0;
    font-size: 0.78rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: #8a8a93;
  }
  .panel .x {
    background: transparent;
    border: none;
    color: #8a8a93;
    font-size: 0.95rem;
    cursor: pointer;
  }
  .panel-body {
    padding: 0.9rem;
    overflow-y: auto;
  }
</style>
