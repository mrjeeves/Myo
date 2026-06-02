<script lang="ts">
  // The conversation stage: the ongoing relationship, turn by turn. User words,
  // a subtle "recalled from memory" cue, the streamed answer, and the live tool
  // activity beneath each turn.
  import { myo } from "../lib/stage.svelte";
  import Activity from "./Activity.svelte";
  import LoadingPulse from "./LoadingPulse.svelte";

  let scroller: HTMLDivElement;

  // Keep the latest turn in view as it streams. Touch every part that can grow
  // — assistant text, live transcript, the tool feed (often the tallest),
  // reasoning, and errors — so the scroll follows all of them, not just text.
  $effect(() => {
    const last = myo.turns.at(-1);
    void myo.turns.length;
    void last?.assistant;
    void last?.partial;
    void last?.thinking;
    void last?.error;
    void last?.activity.length;
    void myo.liveTranscript; // follow the live dictation caption too
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
  });
</script>

<div class="conversation" bind:this={scroller}>
  {#if myo.turns.length === 0 && !myo.liveTranscript}
    <div class="empty">
      <p>{myo.asrStatus || "Say hello — Myo is listening."}</p>
      <p class="hint">It remembers across days, runs on your machine, and shows you its work.</p>
    </div>
  {/if}

  {#each myo.turns as turn (turn.id)}
    <div class="turn">
      {#if turn.userText || turn.partial}
        <div class="user">{turn.userText || turn.partial}</div>
      {/if}

      {#if turn.memories.length}
        <div class="recall" title="Myo drew on what it remembers about you">
          ↻ recalled {turn.memories.length} from memory
        </div>
      {/if}

      <Activity {turn} />

      {#if turn.assistant}
        <div class="assistant">{turn.assistant}</div>
      {:else if !turn.done && !turn.error && turn.activity.length === 0}
        <!-- Awaiting the first token: model loading (cold start) or slow
             inference. The pulse is the "not frozen" reassurance. -->
        <LoadingPulse loadingModel={!myo.modelLikelyResident} />
      {/if}
      {#if turn.error}
        <div class="error">{turn.error}</div>
      {/if}
    </div>
  {/each}

  {#if myo.liveTranscript}
    <!-- Live dictation: the words forming now, before the utterance finalizes
         into a turn. Firms into a real user bubble the moment it's final. -->
    <div class="turn">
      <div class="user live">{myo.liveTranscript}</div>
    </div>
  {/if}
</div>

<style>
  .conversation {
    flex: 1;
    overflow-y: auto;
    padding: 0.9rem;
    display: flex;
    flex-direction: column;
    gap: 0.9rem;
  }
  .empty {
    margin: auto;
    text-align: center;
    color: #6f6f78;
  }
  .empty p {
    margin: 0.25rem 0;
  }
  .empty .hint {
    font-size: 0.76rem;
    max-width: 22rem;
  }
  .turn {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }
  .user {
    align-self: flex-end;
    max-width: 85%;
    background: #1f2a3a;
    border: 1px solid #2a3b48;
    border-radius: 12px 12px 3px 12px;
    padding: 0.45rem 0.7rem;
    font-size: 0.86rem;
  }
  /* The live dictation caption — dimmed + italic until it finalizes, with a
     soft blinking caret so it reads as "still being heard". */
  .user.live {
    background: #16202c;
    border-style: dashed;
    border-color: #2c4154;
    color: #aebfcc;
    font-style: italic;
  }
  .user.live::after {
    content: "▍";
    margin-left: 0.1rem;
    animation: caret 1.1s step-end infinite;
  }
  @keyframes caret {
    50% {
      opacity: 0;
    }
  }
  .assistant {
    align-self: flex-start;
    max-width: 92%;
    white-space: pre-wrap;
    line-height: 1.5;
    font-size: 0.9rem;
    color: #ededed;
  }
  .recall {
    align-self: flex-start;
    font-size: 0.7rem;
    color: #8a7fb0;
  }
  .error {
    align-self: flex-start;
    color: #e07a7a;
    font-size: 0.8rem;
    background: #2a1416;
    border: 1px solid #3a1e22;
    border-radius: 8px;
    padding: 0.4rem 0.6rem;
  }
</style>
