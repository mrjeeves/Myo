<script lang="ts">
  // The ambient presence indicator — Myo's "face". Not a press-to-talk button:
  // it just reflects whether Myo is idle, listening, thinking, or speaking.
  import { myo } from "../lib/stage.svelte";

  const labels: Record<string, string> = {
    idle: "Ready",
    listening: "Listening",
    thinking: "Thinking",
    speaking: "Speaking",
  };
</script>

<div class="presence" data-phase={myo.phase} title={labels[myo.phase]}>
  <span class="orb"></span>
  <span class="label">{labels[myo.phase]}</span>
</div>

<style>
  .presence {
    display: inline-flex;
    align-items: center;
    gap: 0.45rem;
  }
  .orb {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: #4a4a55;
    box-shadow: 0 0 0 0 rgba(0, 0, 0, 0);
  }
  .label {
    font-size: 0.74rem;
    color: #8a8a93;
    letter-spacing: 0.02em;
  }
  /* idle: still gray. listening: blue. thinking: amber pulse. speaking: green pulse. */
  .presence[data-phase="listening"] .orb {
    background: #5b9bd5;
    animation: pulse 1.6s ease-in-out infinite;
  }
  .presence[data-phase="thinking"] .orb {
    background: #d6b25a;
    animation: pulse 1s ease-in-out infinite;
  }
  .presence[data-phase="speaking"] .orb {
    background: #7dd39b;
    animation: pulse 0.8s ease-in-out infinite;
  }
  @keyframes pulse {
    0% {
      box-shadow: 0 0 0 0 rgba(125, 211, 155, 0.5);
    }
    70% {
      box-shadow: 0 0 0 7px rgba(125, 211, 155, 0);
    }
    100% {
      box-shadow: 0 0 0 0 rgba(125, 211, 155, 0);
    }
  }
</style>
