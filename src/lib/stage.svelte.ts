// The shell's single reactive model — the "stage" the dissolved UI renders onto.
//
// It subscribes to the whole `myo://` intent stream and folds it into runes
// state; every surface reads from this one store and calls its actions. There
// is no fixed navigation: surfaces (assistant text, tool activity, document
// artifacts, agent-opened panels) materialize from what the agent emits.

import {
  api,
  listenMyo,
  type ActivityEvent,
  type ArtifactEvent,
  type AssistantEvent,
  type AudioEvent,
  type Capabilities,
  type EngineEvent,
  type EnginesStatus,
  type ModelLoadEntry,
  type ProgressEvent,
  type TranscriptEvent,
  type TurnId,
  type UiEvent,
} from "./core-api";
import { Listener, StreamingListener, Voice, blobToBase64 } from "./audio-io";

export type Phase = "idle" | "listening" | "thinking" | "speaking";

export interface ToolActivity {
  tool: string;
  phase: "start" | "progress" | "output";
  detail: string;
  imageUrl?: string;
  exitCode?: number | null;
}

export interface MemoryHit {
  text: string;
}

export interface Turn {
  id: TurnId;
  userText: string;
  partial: string;
  assistant: string;
  thinking: string;
  done: boolean;
  error?: string;
  activity: ToolActivity[];
  memories: MemoryHit[];
}

export interface Artifact {
  turn: TurnId;
  title: string;
  language: string;
  content: string;
  docId?: string;
  version?: number;
}

export interface MemoryItem {
  id?: string;
  text?: string;
  category?: string;
  [k: string]: unknown;
}

const MAX_TURNS = 50;
const MAX_ACTIVITY = 60;

// Barge-in tuning. Myo talks straight through a stray word or two (echo bleed, a
// quick "mm-hm"); she only yields the floor when you *keep* talking over her for
// this long while she's speaking. A pause longer than the gap restarts the
// window, so the talk-over has to be sustained — not just a word now and a word
// later — which also makes it robust to imperfect echo cancellation.
const BARGE_IN_MS = 2000;
const BARGE_SUSTAIN_GAP_MS = 600;

function blankTurn(id: TurnId): Turn {
  return {
    id,
    userText: "",
    partial: "",
    assistant: "",
    thinking: "",
    done: false,
    activity: [],
    memories: [],
  };
}

class MyoStore {
  // Engines
  engines = $state<EnginesStatus>({ odysseus: false, myownllm: false });
  engineLog = $state<EngineEvent[]>([]);

  // Settings
  capabilities = $state<Capabilities>({
    web: true,
    files: false,
    code: false,
    reach_out: false,
  });
  incognito = $state(false);

  // Conversation
  phase = $state<Phase>("idle");
  micReady = $state(false);
  // Whether the mic is actively capturing right now (drives the composer's mic
  // button). Distinct from `micReady`, which only means a device exists.
  listening = $state(false);
  // Hard-muted: the user tapped the mic off. Myo's default is always-listening,
  // so this is the one-tap privacy switch, not the norm.
  muted = $state(false);
  // The live "typing" caption from streaming dictation — what Myo is hearing
  // right now, before the utterance finalizes into a turn. Empty when idle.
  liveTranscript = $state("");
  // Transient engine subtitle from the ASR stream ("Loading model…", etc.).
  asrStatus = $state("");
  // Models the engine is downloading/loading right now (force-load progress).
  // Drives the inline progress bar; empty when nothing is being acquired.
  modelLoads = $state<ModelLoadEntry[]>([]);
  turns = $state<Turn[]>([]);

  // Document artifacts (the focal "stage" surface) + recall history
  artifacts = $state<Artifact[]>([]);
  focusedArtifact = $state<number | null>(null);

  // Agent-opened side panel (ui_control open_panel) — "control" | "memory" | …
  openPanel = $state<string | null>(null);

  // Memory surface
  memories = $state<MemoryItem[]>([]);

  // Brain surface: Myo's persona (system prompt). Null until first loaded.
  persona = $state<import("./core-api").PersonaInfo | null>(null);

  private voice = new Voice();
  // Two capture modes, mutually exclusive: `streamer` is the preferred
  // real-time WebSocket dictation; `listener` is the clip/energy-VAD fallback.
  private streamer?: StreamingListener;
  private listener?: Listener;
  // Generation is **single-flight**: at most one reply is ever generated at a
  // time (the accumulator's drain gates on this being empty — see `tryDrain`).
  // Kept as a set so the phase machinery can cheaply ask "is anything generating?"
  // — an entry is added when a turn opens and removed once it produces audio,
  // finishes empty, or errors; the phase reads "thinking" while it's non-empty.
  private generating = new Set<TurnId>();
  // Replies are voiced one at a time, in the order they come back, so Myo never
  // talks over herself. `audioPlaying` plus a `playToken` (bumped to abandon the
  // clip in progress on barge-in) drive the "speaking" phase.
  private audioQueue: AudioEvent[] = [];
  private audioPlaying = false;
  private playToken = 0;
  // Barge-in watermark: turns whose id is at or below this don't get voiced. Set
  // to the newest in-flight turn when the user takes the floor, so replies
  // already on their way finish silently (still streamed + remembered) rather
  // than talking over the user; newer turns (what the user just said) still speak.
  private speakMuteThrough = 0;
  // The highest turn id seen, so a barge-in can mute exactly what's in flight now.
  private lastTurnId = 0;
  // Sustained-talk-over tracking for barge-in (timestamps in ms; 0 = no window).
  private talkOverStart = 0;
  private lastUserVoiceAt = 0;
  // ── The draining accumulator ────────────────────────────────────────────────
  // The user's finalized-but-unprocessed speech. While Myo is generating a reply
  // we don't open a competing turn — we just keep transcribing into here. The
  // instant generation frees up, if there's text waiting it drains as one turn;
  // if there isn't, Myo sits. That's what keeps the back-and-forth natural: a
  // sentence said across a pause isn't chopped into separate turns, and talking
  // while she thinks accumulates into her next reply instead of piling up.
  private accumulator = "";
  // True from the moment a drain decides to open a turn until that turn is
  // registered as generating — it closes the `await` gap in `runUserTurn` so two
  // finals landing back-to-back can't both slip a turn through and break
  // single-flight.
  private opening = false;
  // The artifact currently being streamed (target for delta/update), tracked
  // separately from `focusedArtifact` (what's on the stage) so recalling an
  // older doc mid-stream can't corrupt it.
  private streamingArtifact: number | null = null;
  // Ids for synthetic (client-only) error turns — negative so they never
  // collide with real, positive turn ids from the backend.
  private nextSyntheticTurn = -1;
  private unlisten?: () => void;
  // Session-scoped hint for the loading indicator: has the brain produced any
  // output yet? Until it has, the first turn's wait is most likely a one-time
  // model load (cold start), so the pulse says "loading the model" rather than
  // the generic "working on it". (Ported from MyOwnLLM's model-residency hint.)
  private brainHasSpoken = false;

  /** Wire up the stream and pull initial state. Call once on mount. */
  async init() {
    this.unlisten = await listenMyo({
      assistant: (e) => this.onAssistant(e),
      transcript: (e) => this.onTranscript(e),
      activity: (e) => this.onActivity(e),
      artifact: (e) => this.onArtifact(e),
      ui: (e) => this.onUi(e),
      progress: (e) => this.onProgress(e),
      audio: (e) => this.onAudio(e),
      engine: (e) => this.onEngine(e),
    });

    try {
      const s = await api.settingsGet();
      this.capabilities = s.capabilities;
      this.incognito = s.incognito;
      this.engines = await api.enginesStatus();
    } catch {
      // Backend not ready yet; the engine stream will fill these in.
    }

    // Kick the engines (best-effort; narrated on myo://engine).
    api.enginesEnsureReady().catch(() => {});

    // Myo is always listening: open the mic now. If the WebView needs a user
    // gesture first (autoplay / permission), this throws and the mic button in
    // the composer becomes the enable affordance instead.
    void this.startListening();
  }

  dispose() {
    this.unlisten?.();
    this.voice.stop();
    this.streamer?.stop();
    this.listener?.stop();
  }

  // ── Listening (always-on mic) ────────────────────────────────────────────────

  /** Recompute the phase from the live state — the single source of truth now
   *  that turns overlap. Precedence: speaking (a reply is playing) over thinking
   *  (a reply is still generating) over listening (mic live) over idle. Also
   *  (re)sets the half-duplex gate: the clip listener is deaf while Myo is busy
   *  so it can't transcribe her own voice; the streamer is full-duplex and never
   *  gated here. */
  private syncPhase() {
    if (this.audioPlaying) this.phase = "speaking";
    else if (this.generating.size > 0) this.phase = "thinking";
    else if ((this.streamer?.running || this.listener?.running) && !this.muted)
      this.phase = "listening";
    else this.phase = "idle";
    this.listener?.setGated(this.phase === "thinking" || this.phase === "speaking");
  }

  private gateMic(on: boolean) {
    // Only the clip listener gates — it endpoints client-side, so it must not
    // hear Myo's own reply. Streaming hands endpointing to the engine and runs
    // full-duplex, so it's deliberately never gated here.
    this.listener?.setGated(on);
  }

  /** Open the mic and start the always-on listen loop (best-effort). Prefers
   *  real-time streaming dictation; falls back to the clip listener if the
   *  engine's WebSocket can't be reached. */
  async startListening() {
    if (this.streamer?.running || this.listener?.running) return;
    try {
      await this.startStreaming();
      return;
    } catch (e) {
      console.warn("[myo] streaming dictation unavailable, falling back to clip capture:", e);
    }
    await this.startClipListening();
  }

  /** Real-time path: stream PCM to the engine and render interim + final
   *  captions. Resolves once the mic is live (the socket attaches on its own,
   *  retrying a still-booting engine); throws only on mic permission denial. */
  private async startStreaming() {
    const url = await api.asrStreamUrl();
    const streamer = new StreamingListener({
      url,
      onInterim: (text) => this.onInterim(text),
      onFinal: (text) => this.onFinal(text),
      onSpeechStart: () => this.onStreamSpeechStart(),
      onStatus: (s) => this.onAsrStatus(s),
      onLevel: (rms) => console.debug("[myo] idle mic level", rms.toFixed(4)),
      onError: (e) => this.onStreamError(e),
    });
    await streamer.start();
    this.streamer = streamer;
    this.micReady = true;
    this.listening = true;
    this.muted = false;
    this.syncPhase();
  }

  /** Fallback path: the energy-VAD clip listener (one WAV POST per utterance). */
  private async startClipListening() {
    if (this.listener?.running) return;
    const listener =
      this.listener ??
      new Listener({
        onUtterance: (wav) => void this.onUtterance(wav),
        onSpeechStart: () => this.onSpeechStart(),
        onLevel: (rms) => console.debug("[myo] idle mic level", rms.toFixed(4)),
        onError: (e) => {
          console.warn("[myo] mic error", e);
          this.micReady = false;
          this.listening = false;
          this.listener = undefined;
          this.syncPhase();
        },
      });
    try {
      await listener.start();
      this.listener = listener;
      this.micReady = true;
      this.listening = true;
      this.muted = false;
      this.syncPhase();
    } catch (e) {
      console.warn("[myo] could not start listening (falling back to typing):", e);
      this.micReady = false;
    }
  }

  // ── Streaming-dictation handlers ───────────────────────────────────────────

  /** A new utterance is forming. Feed the barge-in monitor (the user starting to
   *  talk is the first beat of a possible talk-over) and light up "listening"
   *  when Myo is otherwise free. */
  private onStreamSpeechStart() {
    this.noteUserVoiceActivity();
    if (this.generating.size === 0 && !this.audioPlaying && !this.muted) {
      this.syncPhase();
    }
  }

  /** Live caption refines as the user speaks. Show it appended to whatever's
   *  already accumulated, so the running thought stays visible even while Myo is
   *  busy generating; also count each refinement as "still talking" for barge-in. */
  private onInterim(text: string) {
    this.liveTranscript = this.accumulator ? `${this.accumulator} ${text}` : text;
    this.noteUserVoiceActivity();
  }

  /** A finalized utterance segment. Fold it into the accumulator (the running,
   *  unprocessed transcription) and try to drain it — see `tryDrain` for the rule.
   *  Never cancels a generation; at most it opens the *next* one. The text stays
   *  visible as the live caption until it drains into its own turn. */
  private onFinal(text: string) {
    this.talkOverStart = 0; // the segment closed; reopen the barge-in window
    const seg = text.trim();
    if (seg) this.accumulator = this.accumulator ? `${this.accumulator} ${seg}` : seg;
    this.liveTranscript = this.accumulator;
    this.tryDrain();
  }

  /** Engine subtitle off the stream ("Loading model…", "Listening…", errors). */
  private onAsrStatus(status: string) {
    this.asrStatus = status;
  }

  /** The streaming socket gave up (engine unreachable / fatal error). Drop it
   *  and fall back to the clip listener so Myo keeps her ears either way. */
  private onStreamError(e: unknown) {
    console.warn("[myo] streaming dictation lost; switching to clip capture:", e);
    this.streamer?.stop();
    this.streamer = undefined;
    this.liveTranscript = "";
    this.accumulator = "";
    this.asrStatus = "";
    if (!this.muted) void this.startClipListening();
  }

  /** Hard mute: release the mic entirely (privacy switch). */
  stopListening() {
    this.streamer?.stop();
    this.streamer = undefined;
    this.listener?.stop();
    this.listener = undefined;
    this.listening = false;
    this.liveTranscript = "";
    this.accumulator = ""; // privacy switch: drop anything heard-but-not-yet-sent
  }

  /** The composer's mic button: toggle always-on listening on/off. */
  async toggleMic() {
    // Whichever capture mode is live (streaming or clip); both expose the same
    // running/suspended/resume surface.
    const active = this.streamer ?? this.listener;
    if (active?.running) {
      if (active.suspended) {
        // First user gesture: unlock a context that started suspended (autoplay
        // policy) rather than muting — so the button reliably *enables* the ears.
        await active.resume();
        this.syncPhase();
        return;
      }
      this.stopListening();
      this.muted = true;
      this.syncPhase();
    } else {
      this.muted = false;
      await this.startListening();
    }
  }

  private onSpeechStart() {
    // The clip listener is gated (half-duplex) while Myo thinks/speaks, so onset
    // only fires when she's free — reflect that she's actively hearing you.
    if (this.generating.size === 0 && !this.audioPlaying && !this.muted) {
      this.syncPhase();
    }
  }

  private async onUtterance(wav: Blob) {
    // The clip path is half-duplex: gate first (before any await) so it can't
    // capture Myo's own reply while this turn works. No cancel and no hush — a
    // new utterance opens another turn alongside any already in flight.
    this.gateMic(true);
    this.phase = "thinking";
    try {
      const b64 = await blobToBase64(wav);
      const turn = await api.feedAudio(b64, "audio/wav");
      if (turn != null) this.noteTurn(turn);
      else this.syncPhase(); // nothing intelligible (silence / noise)
    } catch (e) {
      console.error("[myo] transcription failed", e);
      this.pushErrorTurn(String(e));
      this.syncPhase();
    }
  }

  // ── Actions ────────────────────────────────────────────────────────────────

  /** The text composer. Typed input joins the same accumulator as dictation, so
   *  it honours single-flight (it queues behind an in-flight reply rather than
   *  racing it) and coalesces with anything said in the same beat. */
  async say(text: string) {
    const t = text.trim();
    if (!t) return;
    this.accumulator = this.accumulator ? `${this.accumulator} ${t}` : t;
    this.tryDrain();
  }

  /** The drain rule (the heart of the natural back-and-forth):
   *   • a reply is generating (or one is mid-open) → keep accumulating; just transcribe.
   *   • nothing generating and there's unprocessed text → send it as one turn.
   *   • nothing generating and nothing to say → sit.
   *  Called whenever the accumulator grows (a final lands / text is typed) or
   *  generation frees up (a reply finishes), so the floor is taken the instant
   *  it's both clear and there's something to say. */
  private tryDrain() {
    if (this.opening || this.generating.size > 0) return; // inference busy → transcribe
    const text = this.accumulator.trim();
    if (!text) return; // nothing unprocessed → sit
    this.accumulator = "";
    this.liveTranscript = ""; // it's a committed turn now, not a live caption
    this.opening = true;
    void this.runUserTurn(text);
  }

  /** Open one user turn — the drain target. Single-flight is already guaranteed
   *  by `tryDrain` (it only fires when the floor is clear and sets `opening`), so
   *  this never cancels or hushes; it just fires the brain→TTS round-trip. */
  private async runUserTurn(text: string) {
    try {
      const turn = await api.say(text);
      this.noteTurn(turn);
    } catch (e) {
      this.pushErrorTurn(String(e));
      this.syncPhase();
    } finally {
      this.opening = false;
      // More may have accumulated while we were opening; if the turn failed,
      // nothing is generating so this drains it — otherwise it harmlessly no-ops.
      this.tryDrain();
    }
  }

  /** Stop Myo *talking* right now — the manual barge-in behind the composer's
   *  Stop button, and the same thing a sustained talk-over triggers. It hushes
   *  playback, drops anything queued to be voiced, and silences the replies
   *  already in flight (they still finish and are remembered — Myo just won't
   *  speak them). It deliberately does NOT cancel the generations: each turn
   *  carries the whole conversation, so there's nothing to gain by killing one. */
  hush() {
    this.audioQueue = [];
    this.speakMuteThrough = this.lastTurnId;
    this.playToken++; // make the in-flight pump bow out without resetting state
    this.audioPlaying = false;
    this.voice.stop();
    this.talkOverStart = 0;
    this.syncPhase();
  }

  // ── Voicing queue + barge-in ───────────────────────────────────────────────

  /** Track a freshly opened turn: it's now generating, and it's the newest turn
   *  id (so a later barge-in knows exactly how far to mute). */
  private noteTurn(turn: TurnId) {
    this.generating.add(turn);
    if (turn > this.lastTurnId) this.lastTurnId = turn;
    this.syncPhase();
  }

  /** Fold one beat of user speech into the barge-in window. While Myo is
   *  speaking, *sustained* talk-over (continuous voice for `BARGE_IN_MS`, with no
   *  gap longer than `BARGE_SUSTAIN_GAP_MS`) is her cue to stop. While she's not
   *  speaking, talking never interrupts — it only opens more turns. */
  private noteUserVoiceActivity() {
    const now = performance.now();
    if (!this.audioPlaying) {
      this.talkOverStart = 0;
      this.lastUserVoiceAt = now;
      return;
    }
    // A gap since the last beat means the user paused — restart the window so
    // only continuous talk-over counts toward the threshold.
    if (this.talkOverStart === 0 || now - this.lastUserVoiceAt > BARGE_SUSTAIN_GAP_MS) {
      this.talkOverStart = now;
    }
    this.lastUserVoiceAt = now;
    if (now - this.talkOverStart >= BARGE_IN_MS) this.hush();
  }

  /** Queue a turn's audio for voicing — unless the turn was already barged past
   *  (its id is at or below the mute watermark), in which case it stays silent. */
  private enqueueAudio(e: AudioEvent) {
    if (e.turn <= this.speakMuteThrough) return;
    const hasAudio = (e.kind === "ready" && !!e.b64) || (e.kind === "speak" && !!e.text);
    if (!hasAudio) return;
    this.audioQueue.push(e);
    void this.pumpAudio();
  }

  /** Drain the voicing queue one clip at a time so replies never overlap. Only
   *  one pump runs at once (guarded by `audioPlaying`); a barge-in bumps
   *  `playToken`, which makes the in-flight pump bow out quietly instead of
   *  clobbering the state `hush()` just reset. */
  private async pumpAudio() {
    if (this.audioPlaying) return;
    this.audioPlaying = true;
    const token = ++this.playToken;
    this.syncPhase(); // → speaking
    while (this.audioQueue.length && token === this.playToken) {
      const e = this.audioQueue.shift()!;
      if (e.turn <= this.speakMuteThrough) continue; // barged past while queued
      if (e.kind === "ready" && e.b64) {
        await this.voice.playBase64(e.b64, e.mime ?? "audio/mpeg");
      } else if (e.kind === "speak" && e.text) {
        await this.voice.speakAsync(e.text);
      }
    }
    if (token === this.playToken) {
      this.audioPlaying = false;
      this.syncPhase();
    }
  }

  /** User flips a capability: reflect it and push to the brain. */
  async setCapability(key: keyof Capabilities, value: boolean) {
    const next = { ...this.capabilities, [key]: value };
    this.capabilities = next;
    try {
      this.capabilities = await api.capabilitiesSet(next);
    } catch {
      // Keep the optimistic value; it's persisted locally regardless.
    }
  }

  async setIncognito(on: boolean) {
    this.incognito = on;
    await api.setIncognito(on).catch(() => {});
  }

  async loadMemories(query?: string) {
    try {
      const resp = (await api.memoryList(query)) as Record<string, unknown>;
      const list = (resp?.memory ?? resp?.memories ?? []) as MemoryItem[];
      this.memories = Array.isArray(list) ? list : [];
    } catch {
      this.memories = [];
    }
  }

  async forgetMemory(id: string) {
    await api.memoryForget(id).catch(() => {});
    this.memories = this.memories.filter((m) => m.id !== id);
  }

  /** Load Myo's persona (the system prompt) for the Brain surface. */
  async loadPersona() {
    try {
      this.persona = await api.personaGet();
    } catch {
      this.persona = null;
    }
  }

  /** Save a custom persona; an empty string resets to the built-in default. */
  async savePersona(text: string) {
    try {
      this.persona = await api.personaSet(text);
    } catch {
      // Backend unreachable — keep the last-known persona on screen.
    }
  }

  /** Clear the override → back to the built-in default persona. */
  async resetPersona() {
    await this.savePersona("");
  }

  recallArtifact(index: number) {
    if (index >= 0 && index < this.artifacts.length) this.focusedArtifact = index;
  }

  closeArtifact() {
    this.focusedArtifact = null;
  }

  showPanel(name: string | null) {
    this.openPanel = name;
  }

  setMicReady(ready: boolean) {
    this.micReady = ready;
  }

  get focused(): Artifact | null {
    return this.focusedArtifact === null
      ? null
      : (this.artifacts[this.focusedArtifact] ?? null);
  }

  /** Best-guess: has the chat model loaded at least once this session? Drives
   *  the inline loading copy — generic "working on it" vs cold "loading the
   *  model" — so the very first turn's wait reads as a one-time load. */
  get modelLikelyResident(): boolean {
    return this.brainHasSpoken;
  }

  // ── Stream reducer ───────────────────────────────────────────────────────

  private turnById(id: TurnId): Turn {
    if (id > this.lastTurnId) this.lastTurnId = id;
    let t = this.turns.find((x) => x.id === id);
    if (!t) {
      t = blankTurn(id);
      this.turns.push(t);
      if (this.turns.length > MAX_TURNS) this.turns.splice(0, this.turns.length - MAX_TURNS);
    }
    return t;
  }

  private onAssistant(e: AssistantEvent) {
    const t = this.turnById(e.turn);
    if (e.kind === "delta" && e.text) {
      t.assistant += e.text;
      this.brainHasSpoken = true; // the model produced output → it's resident
    } else if (e.kind === "done") {
      t.done = true;
      // A reply with text gets voiced next (AudioReady/AudioSpeak follows and
      // settles the turn); one with nothing to say is finished right here.
      if (!t.assistant.trim()) {
        this.generating.delete(e.turn);
        this.syncPhase();
        this.tryDrain(); // inference free → send anything that piled up while it ran
      }
    }
  }

  private onTranscript(e: TranscriptEvent) {
    const t = this.turnById(e.turn);
    if (e.kind === "final") {
      t.userText = e.text;
      t.partial = "";
    } else {
      t.partial = e.text;
    }
  }

  private onActivity(e: ActivityEvent) {
    const t = this.turnById(e.turn);
    const detail =
      e.phase === "output" ? (e.output ?? "") : (e.command ?? e.progress ?? "");
    t.activity.push({
      tool: e.tool,
      phase: e.phase,
      detail,
      imageUrl: e.image_url ?? undefined,
      exitCode: e.exit_code ?? undefined,
    });
    if (t.activity.length > MAX_ACTIVITY) {
      t.activity.splice(0, t.activity.length - MAX_ACTIVITY);
    }
  }

  private onArtifact(e: ArtifactEvent) {
    if (e.kind === "open") {
      this.artifacts.push({
        turn: e.turn,
        title: e.title ?? "Document",
        language: e.language ?? "markdown",
        content: "",
      });
      // A freshly streaming doc takes the stage and becomes the stream target.
      this.streamingArtifact = this.artifacts.length - 1;
      this.focusedArtifact = this.streamingArtifact;
      return;
    }
    // Target the streaming artifact, NOT the focused one — the user may have
    // recalled an older doc onto the stage while this keeps streaming.
    const idx = this.streamingArtifact ?? this.artifacts.length - 1;
    const art = this.artifacts[idx];
    if (!art) return;
    if (e.kind === "delta" && e.content != null) {
      art.content = e.content; // Odysseus streams full-content-so-far
    } else if (e.kind === "update") {
      if (e.content != null) art.content = e.content;
      if (e.title != null) art.title = e.title;
      if (e.language != null) art.language = e.language;
      if (e.doc_id != null) art.docId = e.doc_id;
      if (e.version != null) art.version = e.version;
    }
  }

  private onUi(e: UiEvent) {
    const d = e.data ?? {};
    switch (e.directive) {
      case "open_panel": {
        const panel = String(d.panel ?? "");
        // Map Odysseus panels onto the surfaces Myo has; ignore the rest.
        if (panel === "memories") this.openPanel = "memory";
        else if (panel === "settings") this.openPanel = "settings";
        break;
      }
      case "toggle": {
        // The agent flipped a capability server-side; reflect it locally.
        const name = String(d.toggle_name ?? "");
        const on = Boolean(d.state);
        if (name === "web") this.capabilities = { ...this.capabilities, web: on };
        else if (name === "bash") this.capabilities = { ...this.capabilities, code: on };
        else if (name === "incognito") this.incognito = on;
        break;
      }
      default:
        // Unknown directive — gracefully ignored (PLAN risk #4).
        break;
    }
  }

  private onProgress(e: ProgressEvent) {
    // Engine-level force-load progress (sentinel turn 0): drives the inline
    // download/load bar. Handled before `turnById` so it never spawns a phantom
    // turn for the sentinel id.
    if (e.kind === "model_load") {
      const data = e.data as { active?: ModelLoadEntry[] } | null;
      this.modelLoads = Array.isArray(data?.active) ? data.active : [];
      return;
    }
    const t = this.turnById(e.turn);
    if (e.kind === "thinking") {
      const text = (e.data as { text?: string })?.text ?? "";
      t.thinking += text;
    } else if (e.kind === "memories_used") {
      const hits = (e.data as MemoryHit[]) ?? [];
      if (Array.isArray(hits)) t.memories = hits;
    } else if (e.kind === "error") {
      t.error = (e.data as { message?: string })?.message ?? "error";
      t.done = true;
      this.generating.delete(e.turn);
      this.syncPhase();
      this.tryDrain(); // inference free → send anything that piled up while it ran
    }
  }

  private onAudio(e: AudioEvent) {
    // The brain's work for this turn is done; what's left is voicing it (unless
    // it was barged past). Hand it to the queue so replies are spoken one at a
    // time, in the order they came back, rather than cutting each other off.
    this.generating.delete(e.turn);
    this.enqueueAudio(e);
    this.syncPhase();
    // Generation is free now (only voicing remains) — if the user spoke while
    // this reply was being generated, drain it into the next turn, so Myo can
    // generate her next answer while she's still speaking this one.
    this.tryDrain();
  }

  private onEngine(e: EngineEvent) {
    this.engineLog.push(e);
    if (this.engineLog.length > 40) this.engineLog.splice(0, this.engineLog.length - 40);
    const up = e.status === "healthy" || e.status === "ready";
    const down = ["timeout", "error", "unavailable"].includes(e.status);
    if (e.name === "odysseus") {
      if (up) this.engines = { ...this.engines, odysseus: true };
      else if (down) this.engines = { ...this.engines, odysseus: false };
    } else if (e.name === "myownllm") {
      if (up) this.engines = { ...this.engines, myownllm: true };
      else if (down) this.engines = { ...this.engines, myownllm: false };
    }
  }

  private pushErrorTurn(message: string) {
    const t = blankTurn(this.nextSyntheticTurn--);
    t.error = message;
    t.done = true;
    this.turns.push(t);
  }
}

/** The one shared store. */
export const myo = new MyoStore();
