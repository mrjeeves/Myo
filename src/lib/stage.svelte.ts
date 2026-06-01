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
import { Voice } from "./audio-io";

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
  turns = $state<Turn[]>([]);

  // Document artifacts (the focal "stage" surface) + recall history
  artifacts = $state<Artifact[]>([]);
  focusedArtifact = $state<number | null>(null);

  // Agent-opened side panel (ui_control open_panel) — "control" | "memory" | …
  openPanel = $state<string | null>(null);

  // Memory surface
  memories = $state<MemoryItem[]>([]);

  private voice = new Voice();
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
        this.phase = "idle";
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
  }

  dispose() {
    this.unlisten?.();
    this.voice.stop();
  }

  // ── Actions ────────────────────────────────────────────────────────────────

  async say(text: string) {
    const t = text.trim();
    if (!t) return;
    // A new utterance while a turn is live is barge-in: cancel the old one
    // first so its bookkeeping isn't orphaned and two turns don't overlap.
    if (this.activeTurn !== null) await this.cancel();
    this.voice.stop();
    this.phase = "thinking";
    try {
      this.activeTurn = await api.say(t);
    } catch (e) {
      this.phase = "idle";
      this.pushErrorTurn(String(e));
    }
  }

  async cancel() {
    this.voice.stop();
    if (this.activeTurn !== null) {
      await api.cancel(this.activeTurn).catch(() => {});
    }
    this.activeTurn = null;
    this.phase = "idle";
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
        this.phase = "idle";
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
        this.phase = "idle";
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
    if (this.phase !== "speaking") this.phase = "idle";
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
