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
  private activeTurn: TurnId | null = null;
  // The artifact currently being streamed (target for delta/update), tracked
  // separately from `focusedArtifact` (what's on the stage) so recalling an
  // older doc mid-stream can't corrupt it.
  private streamingArtifact: number | null = null;
  // Ids for synthetic (client-only) error turns — negative so they never
  // collide with real, positive turn ids from the backend.
  private nextSyntheticTurn = -1;
  private unlisten?: () => void;

  /** Wire up the stream and pull initial state. Call once on mount. */
  async init() {
    this.voice.onStateChange = (s) => {
      if (s === "speaking") {
        this.phase = "speaking";
      } else if (this.activeTurn === null) {
        this.toIdle();
      } else {
        this.phase = "thinking";
      }
    };

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

  /** Phase once a turn settles: keep listening if the mic is live (and not
   *  muted), else idle. Also drops the capture gate raised while Myo was busy,
   *  so her ears reopen the instant she stops thinking/speaking. */
  private toIdle() {
    if ((this.streamer?.running || this.listener?.running) && !this.muted) {
      // Streaming stays full-duplex (no gate); the clip listener reopens its ears.
      this.listener?.setGated(false);
      this.phase = "listening";
    } else {
      this.phase = "idle";
    }
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
      onFinal: (text) => void this.onFinal(text),
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
    if (this.activeTurn === null) this.toIdle();
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
          if (this.activeTurn === null) this.phase = "idle";
        },
      });
    try {
      await listener.start();
      this.listener = listener;
      this.micReady = true;
      this.listening = true;
      this.muted = false;
      if (this.activeTurn === null) this.toIdle();
    } catch (e) {
      console.warn("[myo] could not start listening (falling back to typing):", e);
      this.micReady = false;
    }
  }

  // ── Streaming-dictation handlers ───────────────────────────────────────────

  /** A new utterance is forming. Light up "listening" if idle; the actual
   *  barge-in happens on the *final* (see onFinal), which is robust against
   *  echo-cancellation bleed finalizing a stray word of Myo's own reply. */
  private onStreamSpeechStart() {
    if (this.activeTurn === null && !this.muted && this.phase === "idle") {
      this.phase = "listening";
    }
  }

  /** Live caption refines as the user speaks — replace it in place. */
  private onInterim(text: string) {
    this.liveTranscript = text;
  }

  /** A finalized utterance: run it as a turn. This is full-duplex barge-in —
   *  runUserTurn cancels any in-flight turn and hushes playback first, and does
   *  NOT gate the mic, so Myo keeps listening straight through her own reply. */
  private async onFinal(text: string) {
    this.liveTranscript = "";
    await this.runUserTurn(text, /* gate */ false);
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
        if (this.activeTurn === null) this.toIdle();
        return;
      }
      this.stopListening();
      this.muted = true;
      if (this.activeTurn === null) this.phase = "idle";
    } else {
      this.muted = false;
      await this.startListening();
    }
  }

  private onSpeechStart() {
    // Barge-in hook: the mic is gated while Myo thinks/speaks (half-duplex), so
    // onset only fires when she's idle — reflect that she's actively hearing you.
    if (this.activeTurn === null && !this.muted) this.phase = "listening";
  }

  private async onUtterance(wav: Blob) {
    // Gate first (before any await) so the half-duplex mic can't capture Myo's
    // own reply mid-turn, then close out any stray live turn.
    this.gateMic(true);
    this.voice.stop();
    if (this.activeTurn !== null) {
      await api.cancel(this.activeTurn).catch(() => {});
      this.activeTurn = null;
    }
    this.phase = "thinking";
    try {
      const b64 = await blobToBase64(wav);
      const turn = await api.feedAudio(b64, "audio/wav");
      if (turn == null) {
        // Nothing intelligible (silence / noise) — reopen the ears.
        this.activeTurn = null;
        this.toIdle();
      } else {
        this.activeTurn = turn;
      }
    } catch (e) {
      console.error("[myo] transcription failed", e);
      this.pushErrorTurn(String(e));
      this.activeTurn = null;
      this.toIdle();
    }
  }

  // ── Actions ────────────────────────────────────────────────────────────────

  async say(text: string) {
    // Gate the mic only when NOT streaming: the clip listener would otherwise
    // transcribe Myo's own reply, while streaming runs full-duplex on purpose.
    await this.runUserTurn(text, /* gate */ !this.streamer?.running);
  }

  /** Run one user turn (shared by the text composer and the voice paths): cancel
   *  any in-flight turn (barge-in), hush playback, fire the brain→TTS round-trip.
   *  `gate` mutes the clip listener for the turn; streaming passes `false`. */
  private async runUserTurn(text: string, gate: boolean) {
    const t = text.trim();
    if (!t) return;
    // A new utterance while a turn is live is barge-in: cancel the old one
    // first so its bookkeeping isn't orphaned and two turns don't overlap.
    if (this.activeTurn !== null) await this.cancel();
    if (gate) this.gateMic(true);
    this.voice.stop();
    this.phase = "thinking";
    try {
      this.activeTurn = await api.say(t);
    } catch (e) {
      this.pushErrorTurn(String(e));
      this.toIdle();
    }
  }

  async cancel() {
    this.voice.stop();
    if (this.activeTurn !== null) {
      await api.cancel(this.activeTurn).catch(() => {});
    }
    this.activeTurn = null;
    this.toIdle();
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

  // ── Stream reducer ───────────────────────────────────────────────────────

  private turnById(id: TurnId): Turn {
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
      this.phase = "thinking";
    } else if (e.kind === "done") {
      t.done = true;
      // If there was nothing to voice, the turn is over now.
      if (!t.assistant.trim() && this.activeTurn === e.turn) {
        this.activeTurn = null;
        this.toIdle();
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
      if (this.activeTurn === e.turn) {
        this.activeTurn = null;
        this.toIdle();
      }
    }
  }

  private onAudio(e: AudioEvent) {
    // The brain's work for this turn is finished once we have audio; what
    // remains is playback, which the Voice controller reflects as the
    // "speaking" phase (and then "idle"). Clearing activeTurn now means the
    // post-playback idle callback resolves to "idle", not back to "thinking".
    if (this.activeTurn === e.turn) this.activeTurn = null;
    if (e.kind === "ready" && e.b64) {
      void this.voice.playBase64(e.b64, e.mime ?? "audio/mpeg");
    } else if (e.kind === "speak" && e.text) {
      this.voice.speak(e.text);
    }
    // If no playback actually started (missing audio blob, or no speech engine
    // available), the Voice "speaking" callback never fires — settle to idle
    // now so the UI can't get stuck on "thinking". (When playback DID start,
    // `playBase64`/`speak` already set the phase to "speaking" synchronously.)
    if (this.phase !== "speaking") this.toIdle();
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
