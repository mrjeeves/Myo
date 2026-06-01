<script lang="ts">
  // The ambient "watch it work" feed for one turn: each tool call as it starts,
  // progresses, and finishes — with any inline image/screenshot it produced.
  import type { Turn } from "../lib/stage.svelte";

  let { turn }: { turn: Turn } = $props();
</script>

{#if turn.activity.length}
  <div class="activity">
    {#each turn.activity as a, i (i)}
      <div class="row" class:failed={a.exitCode != null && a.exitCode !== 0}>
        <span class="phase phase-{a.phase}"></span>
        <span class="tool">{a.tool}</span>
        {#if a.detail}<span class="detail">{a.detail}</span>{/if}
      </div>
      {#if a.imageUrl}
        <img class="shot" src={a.imageUrl} alt="output from {a.tool}" />
      {/if}
    {/each}
  </div>
{/if}

<style>
  .activity {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    margin: 0.35rem 0 0.1rem;
    padding-left: 0.2rem;
    border-left: 2px solid #23232c;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.72rem;
    color: #8a8a93;
    padding-left: 0.4rem;
  }
  .phase {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    flex: none;
    background: #5b9bd5;
  }
  .phase-output {
    background: #7dd39b;
  }
  .phase-progress {
    background: #d6b25a;
  }
  .row.failed .phase {
    background: #e07a7a;
  }
  .tool {
    color: #b9c2cc;
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
  }
  .detail {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .shot {
    max-width: 100%;
    border-radius: 6px;
    margin: 0.2rem 0 0.2rem 0.6rem;
    border: 1px solid #23232c;
  }
</style>
