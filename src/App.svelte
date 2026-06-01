<script lang="ts">
  import UpdatesSection from "./surfaces/UpdatesSection.svelte";
</script>

<main>
  <header class="masthead">
    <h1>Myo</h1>
    <p>Your own AI. Local, voice-first — and it keeps itself up to date.</p>
  </header>

  <section class="panel">
    <h2>Updates</h2>
    <UpdatesSection />
  </section>
</main>

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
  main {
    display: flex;
    flex-direction: column;
    gap: 1.25rem;
    padding: 1.25rem;
    min-height: 100vh;
    box-sizing: border-box;
  }
  .masthead h1 {
    margin: 0;
    font-size: 1.6rem;
    letter-spacing: 0.01em;
  }
  .masthead p {
    margin: 0.25rem 0 0;
    color: #8a8a93;
    font-size: 0.82rem;
  }
  .panel h2 {
    margin: 0 0 0.6rem;
    font-size: 0.74rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: #6f6f78;
  }
</style>
