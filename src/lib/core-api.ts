// Typed wrappers over the Myo Core API: the `#[tauri::command]`s the shell
// invokes, and the normalized `myo://` event stream it listens to. This is the
// single place the frontend knows about Tauri — surfaces import from here, not
// from `@tauri-apps/api`.
//
// Shapes mirror `crates/myo-core/src/event.rs` (the `MyoEvent::emit` payloads)
// and the command signatures in `src-tauri/src/core_api.rs`.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type TurnId = number;

export interface Capabilities {
  web: boolean;
  files: boolean;
  code: boolean;
  reach_out: boolean;
}

export interface ShellSettings {
  capabilities: Capabilities;
  incognito: boolean;
}

export interface EnginesStatus {
  odysseus: boolean;
  myownllm: boolean;
}

// ─── Commands ────────────────────────────────────────────────────────────────

export const api = {
  enginesStatus: () => invoke<EnginesStatus>("myo_engines_status"),
  enginesEnsureReady: () => invoke<void>("myo_engines_ensure_ready"),
  /** The engine's live streaming-transcription WebSocket URL (`ws://…/v1/audio/stream`). */
  asrStreamUrl: () => invoke<string>("myo_asr_stream_url"),

  /** Text bypass: returns the allocated turn id. */
  say: (text: string) => invoke<TurnId>("myo_converse_say", { text }),
  cancel: (turn: TurnId) => invoke<boolean>("myo_converse_cancel", { turn }),
  /** Voice path: base64 WAV in → transcribe → turn. `null` = empty transcript. */
  feedAudio: (audio: string, mime: string) =>
    invoke<TurnId | null>("myo_converse_feed_audio", { audio, mime }),
  /** WAV-file bypass (CI hook). `null` = empty transcript. */
  feedWav: (path: string) => invoke<TurnId | null>("myo_converse_feed_wav", { path }),
  setIncognito: (on: boolean) => invoke<boolean>("myo_converse_incognito", { on }),

  capabilitiesGet: () => invoke<Capabilities>("myo_capabilities_get"),
  capabilitiesSet: (caps: Capabilities) =>
    invoke<Capabilities>("myo_capabilities_set", { caps }),

  memoryList: (query?: string) =>
    invoke<unknown>("myo_memory_list", { query: query ?? null }),
  memoryForget: (id: string) => invoke<void>("myo_memory_forget", { id }),

  settingsGet: () => invoke<ShellSettings>("myo_settings_get"),
  ttsSpeak: (text: string) => invoke<TurnId>("myo_tts_speak", { text }),
};

// ─── The normalized `myo://` intent stream ───────────────────────────────────

export interface AssistantEvent {
  turn: TurnId;
  kind: "delta" | "done";
  text?: string;
}
export interface TranscriptEvent {
  turn: TurnId;
  kind: "partial" | "final";
  text: string;
  speaker?: string | null;
}
export interface ActivityEvent {
  turn: TurnId;
  phase: "start" | "progress" | "output";
  tool: string;
  command?: string | null;
  progress?: string | null;
  output?: string | null;
  exit_code?: number | null;
  image_url?: string | null;
  round?: number | null;
}
export interface ArtifactEvent {
  turn: TurnId;
  kind: "open" | "delta" | "update" | "suggestions";
  title?: string | null;
  language?: string | null;
  content?: string | null;
  doc_id?: string | null;
  version?: number | null;
  suggestions?: unknown;
}
export interface UiEvent {
  turn: TurnId;
  directive: string;
  data: Record<string, unknown>;
}
export interface ProgressEvent {
  turn: TurnId;
  kind: string;
  data: unknown;
}
export interface AudioEvent {
  turn: TurnId;
  kind: "ready" | "speak";
  b64?: string;
  mime?: string;
  text?: string;
}
export interface EngineEvent {
  name: string;
  status: string;
  detail?: string | null;
}

/** Handlers for each channel; any subset may be supplied. */
export interface MyoHandlers {
  assistant?: (e: AssistantEvent) => void;
  transcript?: (e: TranscriptEvent) => void;
  activity?: (e: ActivityEvent) => void;
  artifact?: (e: ArtifactEvent) => void;
  ui?: (e: UiEvent) => void;
  progress?: (e: ProgressEvent) => void;
  audio?: (e: AudioEvent) => void;
  engine?: (e: EngineEvent) => void;
}

const CHANNELS: Record<keyof MyoHandlers, string> = {
  assistant: "myo://assistant",
  transcript: "myo://transcript",
  activity: "myo://activity",
  artifact: "myo://artifact",
  ui: "myo://ui",
  progress: "myo://progress",
  audio: "myo://audio",
  engine: "myo://engine",
};

/**
 * Subscribe to every `myo://` channel a handler is given for. Returns a single
 * unlisten that tears them all down.
 */
export async function listenMyo(handlers: MyoHandlers): Promise<UnlistenFn> {
  const unlisteners: UnlistenFn[] = [];
  for (const key of Object.keys(handlers) as (keyof MyoHandlers)[]) {
    const handler = handlers[key];
    if (!handler) continue;
    const un = await listen(CHANNELS[key], (e) =>
      (handler as (p: unknown) => void)(e.payload),
    );
    unlisteners.push(un);
  }
  return () => unlisteners.forEach((u) => u());
}
