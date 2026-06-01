<script lang="ts">
  // Talk to Myo. She's always listening (open-mic), so the mic button is a
  // one-tap hard-mute — not a press-to-talk. Typing stays a first-class
  // fallback. Send while a turn runs becomes Stop (barge-in / cancel).
  import { myo } from "../lib/stage.svelte";

  let text = $state("");

  const busy = $derived(myo.phase === "thinking" || myo.phase === "speaking");
  const listening = $derived(myo.listening);

  function submit(e: Event) {
    e.preventDefault();
    const t = text;
    text = "";
    void myo.say(t);
  }
</script>

<form class="composer" onsubmit={submit}>
  <button
    type="button"
    class="mic"
    class:live={listening}
    class:muted={myo.muted}
    onclick={() => myo.toggleMic()}
    title={listening ? "Listening — tap to mute" : "Muted — tap to listen"}
    aria-label={listening ? "Mute microphone" : "Start listening"}
    aria-pressed={listening}
  >
    {listening ? "🎙" : "🔇"}
  </button>
  <input
    type="text"
    bind:value={text}
    placeholder="Talk to Myo…"
    autocomplete="off"
    spellcheck="false"
  />
  {#if busy}
    <button type="button" class="stop" onclick={() => myo.cancel()}>Stop</button>
  {:else}
    <button type="submit" class="send" disabled={!text.trim()}>Send</button>
  {/if}
</form>

<style>
  .composer {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.6rem 0.5rem;
    border-top: 1px solid #1f1f27;
    background: #0d0d10;
  }
  input {
    flex: 1;
    background: #131318;
    border: 1px solid #23232c;
    border-radius: 8px;
    color: #e8e8e8;
    padding: 0.55rem 0.7rem;
    font: inherit;
    font-size: 0.86rem;
  }
  input:focus {
    outline: none;
    border-color: #3a3a55;
  }
  button {
    border-radius: 8px;
    border: 1px solid #2c2c40;
    background: #1a1a2a;
    color: #e8e8e8;
    padding: 0.5rem 0.7rem;
    font-size: 0.82rem;
    cursor: pointer;
  }
  button:hover:not(:disabled) {
    background: #23233a;
  }
  button:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .mic {
    padding: 0.5rem 0.55rem;
  }
  /* Always-listening: the mic glows. Muted: it dims to a clear "off". */
  .mic.live {
    border-color: #2c5168;
    background: #14222d;
    box-shadow: 0 0 0 0 rgba(91, 155, 213, 0.5);
    animation: mic-pulse 2s ease-in-out infinite;
  }
  .mic.muted {
    opacity: 0.6;
    border-color: #4d2630;
    background: #221416;
  }
  @keyframes mic-pulse {
    0% {
      box-shadow: 0 0 0 0 rgba(91, 155, 213, 0.45);
    }
    70% {
      box-shadow: 0 0 0 6px rgba(91, 155, 213, 0);
    }
    100% {
      box-shadow: 0 0 0 0 rgba(91, 155, 213, 0);
    }
  }
  .send {
    background: #1f3a4d;
    border-color: #2c5168;
    color: #cfe6f2;
  }
  .stop {
    background: #4d1f25;
    border-color: #683038;
    color: #f2cfcf;
  }
</style>
