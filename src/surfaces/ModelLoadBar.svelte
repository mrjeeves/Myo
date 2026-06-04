<script lang="ts">
  // A real, inline progress bar for a model the engine is acquiring on behalf
  // of a force-load: a filled bar with a live percentage while it downloads, an
  // indeterminate shimmer while it loads into memory, and the engine's own
  // status line beneath. Fed by `myo.modelLoads` (the `model_load` progress
  // event the shell relays from MyOwnLLM's `/v1/myownllm/progress`); renders
  // nothing when idle. Sticky so it stays visible while the conversation grows.
  import { myo } from "../lib/stage.svelte";
  import type { ModelLoadEntry } from "../lib/core-api";

  // Friendly noun per engine "kind" so the caption reads for the ear, not the
  // wire (the tag itself is shown quietly as a title on hover).
  function noun(kind: string): string {
    switch (kind) {
      case "chat":
        return "chat model";
      case "embed":
        return "memory model";
      case "speak":
        return "voice model";
      case "transcribe":
        return "speech model";
      default:
        return "model";
    }
  }

  function fmtBytes(n: number): string {
    if (!n) return "";
    const k = 1024;
    if (n >= k * k * k) return `${(n / (k * k * k)).toFixed(2)} GB`;
    if (n >= k * k) return `${(n / (k * k)).toFixed(1)} MB`;
    if (n >= k) return `${Math.round(n / k)} KB`;
    return `${n} B`;
  }

  function pct(e: ModelLoadEntry): number | null {
    return e.percent != null ? Math.round(e.percent * 100) : null;
  }

  function headline(e: ModelLoadEntry): string {
    const n = noun(e.kind);
    if (e.phase === "error") return `Couldn’t load the ${n}`;
    if (e.phase === "loading") return `Loading the ${n} into memory…`;
    if (e.phase === "ready") return `${n[0].toUpperCase()}${n.slice(1)} ready`;
    const p = pct(e);
    return `Downloading the ${n}${p != null ? ` — ${p}%` : "…"}`;
  }

  function caption(e: ModelLoadEntry): string {
    if (e.phase === "downloading" && e.total > 0) {
      return `${fmtBytes(e.completed)} / ${fmtBytes(e.total)}`;
    }
    return e.detail ?? "";
  }
</script>

{#if myo.modelLoads.length}
  <div class="model-loads" aria-live="polite">
    {#each myo.modelLoads as e (e.kind + ":" + e.model)}
      <div class="row" class:error={e.phase === "error"} title={e.model}>
        <div class="head">{headline(e)}</div>
        <div
          class="track"
          role="progressbar"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={pct(e) ?? undefined}
        >
          {#if e.phase === "downloading" && e.percent != null}
            <div class="fill" style="width: {Math.max(2, pct(e) ?? 0)}%"></div>
          {:else if e.phase === "error"}
            <div class="fill err"></div>
          {:else}
            <!-- No byte total (loading into memory, or a pull with no
                 Content-Length yet): an indeterminate sweep, not a fake %. -->
            <div class="fill indet"></div>
          {/if}
        </div>
        {#if caption(e)}
          <div class="cap">{caption(e)}</div>
        {/if}
      </div>
    {/each}
  </div>
{/if}

<style>
  .model-loads {
    position: sticky;
    top: 0;
    z-index: 1;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 0.55rem 0.7rem;
    background: #16202c;
    border: 1px solid #2a3b48;
    border-radius: 10px;
    box-shadow: 0 2px 10px rgba(0, 0, 0, 0.25);
  }
  .row {
    display: flex;
    flex-direction: column;
    gap: 0.28rem;
  }
  .head {
    font-size: 0.78rem;
    color: #cfe0ee;
  }
  .row.error .head {
    color: #e07a7a;
  }
  .track {
    position: relative;
    height: 6px;
    border-radius: 999px;
    background: #22303f;
    overflow: hidden;
  }
  .fill {
    position: absolute;
    inset: 0 auto 0 0;
    height: 100%;
    border-radius: 999px;
    background: linear-gradient(90deg, #4a8fd6, #6fb1f0);
    transition: width 0.3s ease-out;
  }
  .fill.err {
    width: 100%;
    background: #6e2b2f;
    transition: none;
  }
  /* Indeterminate sweep for the no-percentage phases (loading into memory, or a
     download before a total is known) — motion that reads as "working", not a
     stalled bar parked at 0%. */
  .fill.indet {
    width: 35%;
    background: linear-gradient(90deg, transparent, #6fb1f0, transparent);
    animation: indet 1.3s ease-in-out infinite;
  }
  @keyframes indet {
    0% {
      transform: translateX(-120%);
    }
    100% {
      transform: translateX(320%);
    }
  }
  .cap {
    font-size: 0.7rem;
    color: #8aa0b2;
    font-variant-numeric: tabular-nums;
  }
</style>
