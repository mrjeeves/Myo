<script lang="ts">
  // The memory surface: a thing that remembers everything has to be visible,
  // forgettable, and pausable. Review what Myo knows about you, search it, and
  // forget any of it — all local, nothing leaves the device.
  import { onMount } from "svelte";
  import { myo } from "../lib/stage.svelte";

  let query = $state("");

  onMount(() => myo.loadMemories());

  function search(e: Event) {
    e.preventDefault();
    void myo.loadMemories(query.trim() || undefined);
  }
</script>

<div class="memory">
  <form class="search" onsubmit={search}>
    <input type="text" bind:value={query} placeholder="Search what Myo remembers…" />
    <button type="submit">Search</button>
  </form>

  {#if myo.memories.length === 0}
    <p class="empty">Nothing remembered yet.</p>
  {:else}
    <ul>
      {#each myo.memories as m (m.id ?? m.text)}
        <li>
          <div class="text">{m.text ?? "(memory)"}</div>
          <div class="meta">
            {#if m.category}<span class="cat">{m.category}</span>{/if}
            {#if m.id}
              <button class="forget" onclick={() => myo.forgetMemory(m.id!)}>Forget</button>
            {/if}
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .memory {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }
  .search {
    display: flex;
    gap: 0.4rem;
  }
  .search input {
    flex: 1;
    background: #131318;
    border: 1px solid #23232c;
    border-radius: 7px;
    color: #e8e8e8;
    padding: 0.45rem 0.6rem;
    font: inherit;
    font-size: 0.82rem;
  }
  .search input:focus {
    outline: none;
    border-color: #3a3a55;
  }
  button {
    background: #1a1a2a;
    border: 1px solid #2c2c40;
    color: #e8e8e8;
    border-radius: 6px;
    padding: 0.4rem 0.6rem;
    font-size: 0.78rem;
    cursor: pointer;
  }
  button:hover {
    background: #23233a;
  }
  .empty {
    color: #6f6f78;
    font-size: 0.82rem;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  li {
    background: #131318;
    border: 1px solid #1f1f27;
    border-radius: 8px;
    padding: 0.55rem 0.7rem;
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }
  .text {
    font-size: 0.84rem;
    line-height: 1.4;
  }
  .meta {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .cat {
    font-size: 0.68rem;
    color: #6f6f78;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .forget {
    padding: 0.25rem 0.5rem;
    font-size: 0.72rem;
    color: #d6a0a0;
    border-color: #4a2c30;
    background: #2a1a1c;
  }
  .forget:hover {
    background: #3a2226;
  }
</style>
