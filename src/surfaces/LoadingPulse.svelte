<script lang="ts">
  // A calm "still working, not frozen" indicator: a reassurance word that
  // rotates every few seconds behind a moving shine. On a cold start it swaps
  // to "loading the model" phrasing, so an opaque wait reads as a one-time
  // model load rather than a stuck turn. Ported from MyOwnLLM's LoadingPulse
  // (the live CPU/RAM proof-of-life line is omitted — Myo has no usage probe
  // yet; the shining word is the load-bearing part).
  import { onMount, onDestroy } from "svelte";

  let { loadingModel = false }: { loadingModel?: boolean } = $props();

  // Generic "work is underway" phrases — the model is resident, the turn is
  // just taking a moment (slow inference).
  const WORDS = [
    "Working on it…",
    "Thinking it through…",
    "Crunching…",
    "Hang tight…",
    "Still working…",
    "Just a moment…",
    "Almost there…",
  ];
  // Cold-load phrases — the one-time load into memory, so the wait reads as
  // "the model is coming up" rather than "stuck".
  const LOADING_WORDS = [
    "Loading the model…",
    "Warming up the model…",
    "Getting the model ready…",
    "Loading into memory…",
  ];
  const WORD_MS = 3000;

  let wordIdx = $state(0);
  let timer: ReturnType<typeof setInterval> | null = null;

  const words = $derived(loadingModel ? LOADING_WORDS : WORDS);
  const displayWord = $derived(words[wordIdx % words.length]);

  onMount(() => {
    timer = setInterval(() => (wordIdx += 1), WORD_MS);
  });
  onDestroy(() => {
    if (timer) clearInterval(timer);
  });
</script>

<div class="loading-pulse" aria-live="polite">
  {#key displayWord}
    <span class="loading-word">{displayWord}</span>
  {/key}
</div>

<style>
  .loading-pulse {
    align-self: flex-start;
  }
  .loading-word {
    display: inline-block;
    font-size: 0.9rem;
    font-weight: 500;
    background: linear-gradient(
      90deg,
      #6f6f78 0%,
      #6f6f78 38%,
      #e8e8ff 50%,
      #6f6f78 62%,
      #6f6f78 100%
    );
    background-size: 220% 100%;
    -webkit-background-clip: text;
    background-clip: text;
    -webkit-text-fill-color: transparent;
    color: transparent;
    animation:
      loading-word-in 0.4s ease-out,
      loading-shine 2.4s linear infinite;
  }
  @keyframes loading-shine {
    0% {
      background-position: 160% 0;
    }
    100% {
      background-position: -160% 0;
    }
  }
  @keyframes loading-word-in {
    from {
      opacity: 0;
      transform: translateY(2px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }
</style>
