// Audio I/O for the shell — Myo's voice out *and* her ears.
//
// Voice OUT: play the brain's synthesized speech, or fall back to the
// browser's speech engine when no TTS provider is configured. One `Voice`
// instance backs the whole app so barge-in can stop whatever is playing.
//
// Voice IN comes in two flavours, both always-on and both WebView-side (so the
// browser's echo-cancellation sees the same audio graph as the TTS playback,
// and nothing audible ever leaves the device — audio is transcribed and
// dropped, Myo's privacy default):
//
//   • `StreamingListener` (preferred) — opens a WebSocket to the engine's live
//     dictation route and streams 16 kHz mono PCM continuously, reading back
//     *interim* captions as you speak and a *final* per utterance. The engine
//     keeps the model warm and owns the endpointing (Silero VAD), so this is
//     real-time and full-duplex (she can keep listening while she replies).
//   • `Listener` (fallback) — opens the mic and watches for utterances with a
//     lightweight energy VAD (onset → trailing-silence endpoint), handing each
//     finished utterance back as a 16-bit WAV the shell POSTs to the one-shot
//     transcription route. Used when the streaming socket can't be reached.

export type VoiceState = "idle" | "speaking";

export class Voice {
  private audio: HTMLAudioElement | null = null;
  private speaking = false;

  /** Notified whenever playback starts or stops. */
  onStateChange?: (state: VoiceState) => void;

  /** Play base64-encoded audio synthesized by the brain. Resolves when done. */
  async playBase64(b64: string, mime: string): Promise<void> {
    this.stop();
    const audio = new Audio(`data:${mime};base64,${b64}`);
    this.audio = audio;
    this.setSpeaking(true);
    try {
      await audio.play();
      await new Promise<void>((resolve) => {
        audio.onended = () => resolve();
        audio.onerror = () => resolve();
      });
    } catch {
      // Autoplay can be blocked until the user interacts; not fatal.
    } finally {
      if (this.audio === audio) {
        this.audio = null;
        this.setSpeaking(false);
      }
    }
  }

  /** Voice text with the browser's speech engine (the WebSpeech fallback). */
  speak(text: string): void {
    this.stop();
    if (!("speechSynthesis" in window)) return;
    const u = new SpeechSynthesisUtterance(text);
    u.onend = () => this.setSpeaking(false);
    u.onerror = () => this.setSpeaking(false);
    this.setSpeaking(true);
    window.speechSynthesis.speak(u);
  }

  /** Stop whatever is playing — for barge-in or an explicit stop. */
  stop(): void {
    if (this.audio) {
      this.audio.pause();
      this.audio = null;
    }
    if ("speechSynthesis" in window) window.speechSynthesis.cancel();
    this.setSpeaking(false);
  }

  private setSpeaking(s: boolean) {
    if (this.speaking !== s) {
      this.speaking = s;
      this.onStateChange?.(s ? "speaking" : "idle");
    }
  }
}

/** Is a microphone reachable? (Capability probe for the Presence indicator.) */
export async function micAvailable(): Promise<boolean> {
  if (!navigator.mediaDevices?.enumerateDevices) return false;
  try {
    const devices = await navigator.mediaDevices.enumerateDevices();
    return devices.some((d) => d.kind === "audioinput");
  } catch {
    return false;
  }
}

// ─── Voice input: always-on capture + energy VAD ─────────────────────────────

function log(...args: unknown[]) {
  // Verbose by design while the voice loop is young — the first thing you want
  // when "she didn't listen" is the mic/VAD trace in the devtools console.
  console.info("[myo-listen]", ...args);
}

export interface ListenerOptions {
  /** A finished utterance, as a mono 16-bit PCM WAV (at the capture rate). */
  onUtterance: (wav: Blob) => void;
  /** Speech onset — useful for barge-in and lighting up the orb. */
  onSpeechStart?: () => void;
  /** Throttled RMS level (0..1) while idle — handy for tuning the threshold. */
  onLevel?: (rms: number) => void;
  /** Capture failed to start or died (permission denied, device lost, …). */
  onError?: (e: unknown) => void;
}

/**
 * Open-mic capture with a simple, dependency-free energy VAD.
 *
 * A finished utterance is: a stretch that crossed the speech-onset threshold,
 * ran for at least `minUtteranceMs` of voiced audio, and was closed by
 * `endSilenceMs` of trailing quiet (or hit `maxUtteranceMs`). The thresholds
 * are deliberately conservative and easy to tune — `onLevel` reports the idle
 * RMS so you can see where a given mic sits. (Silero-VAD-in-the-browser is the
 * eventual upgrade; this gets the always-listening loop working today.)
 */
export class Listener {
  private ctx?: AudioContext;
  private stream?: MediaStream;
  private source?: MediaStreamAudioSourceNode;
  private processor?: ScriptProcessorNode;
  private sink?: GainNode;
  private sampleRate = 48000;

  // VAD state machine
  private speaking = false;
  private utterance: Float32Array[] = [];
  private utteranceMs = 0;
  private silenceMs = 0;
  private preroll?: Float32Array; // last idle frame, prepended so onsets aren't clipped
  private lastLevelLog = 0;

  // Hard gate: when true, audio is dropped and any in-flight utterance reset.
  // The shell raises this while Myo is thinking/speaking so she never
  // transcribes her own voice (half-duplex open-mic).
  private gated = false;

  // Tuning (RMS is 0..1). Hysteresis: enter speech hotter than you leave it.
  private readonly startRms = 0.014;
  private readonly endRms = 0.008;
  private readonly minUtteranceMs = 320;
  private readonly endSilenceMs = 750;
  private readonly maxUtteranceMs = 15000;

  constructor(private opts: ListenerOptions) {}

  get running(): boolean {
    return !!this.stream;
  }

  /** True when the audio context started suspended (autoplay policy) and is
   *  waiting on a user gesture to resume — capture is silent until then. */
  get suspended(): boolean {
    return this.ctx?.state === "suspended";
  }

  /** Open the mic and begin watching for utterances. Throws on permission
   *  denial / no device so the caller can fall back to type-only. */
  async start(): Promise<void> {
    if (this.stream) return;
    if (!navigator.mediaDevices?.getUserMedia) {
      throw new Error("getUserMedia unavailable in this WebView");
    }
    log("requesting microphone…");
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: {
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
        channelCount: 1,
      },
    });
    this.stream = stream;

    const Ctx =
      window.AudioContext ?? (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
    const ctx = new Ctx();
    this.ctx = ctx;
    this.sampleRate = ctx.sampleRate;
    // Autoplay policy can leave a fresh context suspended until a gesture;
    // resume() here works when start() is itself driven by a click.
    if (ctx.state === "suspended") {
      try {
        await ctx.resume();
      } catch {
        /* a later user gesture (the mic button) will resume it */
      }
    }

    const source = ctx.createMediaStreamSource(stream);
    const processor = ctx.createScriptProcessor(4096, 1, 1);
    // Route through a silent gain so onaudioprocess fires on every browser
    // *without* echoing the mic back to the speakers.
    const sink = ctx.createGain();
    sink.gain.value = 0;
    processor.onaudioprocess = (e) => this.onFrame(e.inputBuffer.getChannelData(0));
    source.connect(processor);
    processor.connect(sink);
    sink.connect(ctx.destination);
    this.source = source;
    this.processor = processor;
    this.sink = sink;

    log(`listening — ${this.sampleRate} Hz, context ${ctx.state}`);
  }

  /** Hard-stop capture and release the mic (the green-LED-off hard mute). */
  stop(): void {
    this.resetUtterance();
    this.processor?.disconnect();
    this.source?.disconnect();
    this.sink?.disconnect();
    this.processor = undefined;
    this.source = undefined;
    this.sink = undefined;
    this.stream?.getTracks().forEach((t) => t.stop());
    this.stream = undefined;
    void this.ctx?.close().catch(() => {});
    this.ctx = undefined;
    log("stopped");
  }

  /** Resume a context that started suspended (call from a user gesture). */
  async resume(): Promise<void> {
    if (this.ctx?.state === "suspended") await this.ctx.resume().catch(() => {});
  }

  /** Drop audio (and any in-flight utterance) until ungated. */
  setGated(gated: boolean): void {
    if (this.gated === gated) return;
    this.gated = gated;
    if (gated) this.resetUtterance();
    log(gated ? "gated (Myo busy)" : "ungated (listening)");
  }

  private onFrame(frame: Float32Array): void {
    if (this.gated) return;
    const frameMs = (frame.length / this.sampleRate) * 1000;
    const rms = computeRms(frame);

    if (!this.speaking) {
      // Idle: report level occasionally, keep a one-frame pre-roll, wait for onset.
      const now = performance.now();
      if (now - this.lastLevelLog > 2000) {
        this.lastLevelLog = now;
        this.opts.onLevel?.(rms);
      }
      if (rms < this.startRms) {
        this.preroll = frame.slice();
        return;
      }
      // Onset.
      this.speaking = true;
      this.utterance = this.preroll ? [this.preroll] : [];
      this.utteranceMs = this.preroll ? (this.preroll.length / this.sampleRate) * 1000 : 0;
      this.silenceMs = 0;
      this.preroll = undefined;
      this.opts.onSpeechStart?.();
      log("speech onset");
    }

    this.utterance.push(frame.slice());
    this.utteranceMs += frameMs;
    this.silenceMs = rms < this.endRms ? this.silenceMs + frameMs : 0;

    if (this.silenceMs >= this.endSilenceMs || this.utteranceMs >= this.maxUtteranceMs) {
      this.finishUtterance();
    }
  }

  private finishUtterance(): void {
    const voicedMs = this.utteranceMs - this.silenceMs;
    const frames = this.utterance;
    this.resetUtterance();
    if (voicedMs < this.minUtteranceMs) {
      log(`utterance too short (${Math.round(voicedMs)}ms voiced) — dropped`);
      return;
    }
    const wav = encodeWav(frames, this.sampleRate);
    log(`utterance: ${Math.round(this.cap(frames))}ms, ${wav.size} bytes`);
    this.opts.onUtterance(wav);
  }

  private cap(frames: Float32Array[]): number {
    const samples = frames.reduce((n, f) => n + f.length, 0);
    return (samples / this.sampleRate) * 1000;
  }

  private resetUtterance(): void {
    this.speaking = false;
    this.utterance = [];
    this.utteranceMs = 0;
    this.silenceMs = 0;
  }
}

function computeRms(frame: Float32Array): number {
  let sum = 0;
  for (let i = 0; i < frame.length; i++) sum += frame[i] * frame[i];
  return Math.sqrt(sum / frame.length);
}

/** Encode mono float32 frames as a 16-bit PCM WAV at `sampleRate`. MyOwnLLM
 *  (via symphonia) reads the header and resamples to 16 kHz, so we send the
 *  capture rate as-is rather than resampling in JS. */
function encodeWav(frames: Float32Array[], sampleRate: number): Blob {
  const length = frames.reduce((n, f) => n + f.length, 0);
  const buffer = new ArrayBuffer(44 + length * 2);
  const view = new DataView(buffer);
  const writeStr = (off: number, s: string) => {
    for (let i = 0; i < s.length; i++) view.setUint8(off + i, s.charCodeAt(i));
  };
  writeStr(0, "RIFF");
  view.setUint32(4, 36 + length * 2, true);
  writeStr(8, "WAVE");
  writeStr(12, "fmt ");
  view.setUint32(16, 16, true); // PCM fmt chunk size
  view.setUint16(20, 1, true); // PCM
  view.setUint16(22, 1, true); // mono
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * 2, true); // byte rate
  view.setUint16(32, 2, true); // block align
  view.setUint16(34, 16, true); // bits per sample
  writeStr(36, "data");
  view.setUint32(40, length * 2, true);
  let off = 44;
  for (const f of frames) {
    for (let i = 0; i < f.length; i++) {
      const s = Math.max(-1, Math.min(1, f[i]));
      view.setInt16(off, s < 0 ? s * 0x8000 : s * 0x7fff, true);
      off += 2;
    }
  }
  return new Blob([buffer], { type: "audio/wav" });
}

/** Base64-encode a Blob's bytes (for handing audio across the Tauri IPC). */
export async function blobToBase64(blob: Blob): Promise<string> {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let binary = "";
  const chunk = 0x8000; // chunked to stay under the String.fromCharCode arg cap
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

// ─── Voice input: real-time streaming dictation (WebSocket) ───────────────────

/** The engine's wire sample rate — 16 kHz mono, the ASR backends' training
 *  rate. Matches MyOwnLLM's `TRANSCRIBE_WS_SAMPLE_RATE`; we resample to it
 *  before sending so the engine never has to. */
const STREAM_SAMPLE_RATE = 16000;

/** One ASR caption segment, as it lands on the wire. `partial` is present and
 *  `true` only while the text is still being refined (the live "typing"
 *  caption); a *finalized* utterance omits it (serde skips `false`). `seg_id`
 *  is stable per utterance on the live path, so interim → final replaces in
 *  place. (Mirrors `EmittedSegment` in MyOwnLLM's `transcribe.rs`.) */
interface StreamSegment {
  text?: string;
  seg_id?: number;
  partial?: boolean;
}

/** A `TranscribeFrame` off the wire. `final` (serde-renamed from `is_final`)
 *  flags the *session* end, not a per-utterance final — that's `partial` on
 *  each segment. `status` carries engine subtitles ("Loading model…",
 *  "Listening…"). Error frames arrive as `{ error }` instead. */
interface StreamFrame {
  segments?: StreamSegment[];
  final?: boolean;
  status?: string;
  error?: string;
}

export interface StreamingOptions {
  /** The engine WS URL, e.g. `ws://127.0.0.1:11473/v1/audio/stream`. */
  url: string;
  /** Interim ("typing") caption for the in-progress utterance — replace in place. */
  onInterim: (text: string, segId: number) => void;
  /** A finalized utterance — the shell runs this as a turn. */
  onFinal: (text: string, segId: number) => void;
  /** First interim of a new utterance (speech onset) — lights up the orb. */
  onSpeechStart?: () => void;
  /** Engine status subtitle ("Loading model…", "Listening…", inference errors). */
  onStatus?: (status: string) => void;
  /** Throttled RMS level (0..1) — for tuning / a level meter. */
  onLevel?: (rms: number) => void;
  /** Streaming gave up (couldn't reach the engine, or a fatal engine error).
   *  The shell falls back to the clip `Listener`. */
  onError?: (e: unknown) => void;
}

const STREAM_MAX_RECONNECTS = 8;

/**
 * Always-on capture that streams PCM to the engine's live dictation socket and
 * surfaces interim + final captions. The mic stays open continuously — the
 * engine owns endpointing (VAD), so unlike `Listener` there's no client-side
 * utterance segmentation. Full-duplex by design: nothing here gates the mic
 * while Myo replies (echo-cancellation keeps her from transcribing herself);
 * `setGated(true)` is the lever to degrade to half-duplex if AEC is weak.
 */
export class StreamingListener {
  private ctx?: AudioContext;
  private stream?: MediaStream;
  private source?: MediaStreamAudioSourceNode;
  private processor?: ScriptProcessorNode;
  private sink?: GainNode;
  private ws?: WebSocket;
  private sampleRate = 48000;

  private closed = false; // hard-stopped by us — suppresses reconnect
  private gated = false; // drop sends (optional half-duplex degrade)
  private reconnects = 0;
  private reconnectTimer?: ReturnType<typeof setTimeout>;
  private lastLevelLog = 0;
  private currentSegId = -1; // utterance in progress (for onSpeechStart edge)

  // Stateful linear resampler (ctx rate → 16 kHz). `tail` carries the unconsumed
  // input across frames and `pos` the fractional read offset, so interpolation
  // spans frame boundaries without clicks.
  private resample = { tail: new Float32Array(0), pos: 0 };

  constructor(private opts: StreamingOptions) {}

  get running(): boolean {
    return !!this.stream;
  }

  get suspended(): boolean {
    return this.ctx?.state === "suspended";
  }

  /** Open the mic and begin streaming. Resolves once capture is live; the WS
   *  attaches (and re-attaches) on its own, so a still-booting engine doesn't
   *  block startup. Throws only on mic permission denial / no device. */
  async start(): Promise<void> {
    if (this.stream) return;
    if (!navigator.mediaDevices?.getUserMedia) {
      throw new Error("getUserMedia unavailable in this WebView");
    }
    log("streaming: requesting microphone…");
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: {
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
        channelCount: 1,
      },
    });
    this.stream = stream;

    const Ctx =
      window.AudioContext ?? (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
    const ctx = new Ctx();
    this.ctx = ctx;
    this.sampleRate = ctx.sampleRate;
    if (ctx.state === "suspended") {
      try {
        await ctx.resume();
      } catch {
        /* a later user gesture (the mic button) will resume it */
      }
    }

    const source = ctx.createMediaStreamSource(stream);
    const processor = ctx.createScriptProcessor(4096, 1, 1);
    const sink = ctx.createGain();
    sink.gain.value = 0; // silent: fire onaudioprocess without echoing to speakers
    processor.onaudioprocess = (e) => this.onFrame(e.inputBuffer.getChannelData(0));
    source.connect(processor);
    processor.connect(sink);
    sink.connect(ctx.destination);
    this.source = source;
    this.processor = processor;
    this.sink = sink;

    log(`streaming — capture ${this.sampleRate} Hz → ${STREAM_SAMPLE_RATE} Hz, context ${ctx.state}`);
    this.connect();
  }

  /** Hard-stop: end the stream, close the socket, release the mic. */
  stop(): void {
    this.closed = true;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.reconnectTimer = undefined;
    const ws = this.ws;
    this.ws = undefined;
    if (ws) {
      try {
        if (ws.readyState === WebSocket.OPEN) ws.send("end");
      } catch {
        /* socket already gone */
      }
      try {
        ws.close();
      } catch {
        /* already closing */
      }
    }
    this.processor?.disconnect();
    this.source?.disconnect();
    this.sink?.disconnect();
    this.processor = undefined;
    this.source = undefined;
    this.sink = undefined;
    this.stream?.getTracks().forEach((t) => t.stop());
    this.stream = undefined;
    void this.ctx?.close().catch(() => {});
    this.ctx = undefined;
    this.resample = { tail: new Float32Array(0), pos: 0 };
    log("streaming: stopped");
  }

  /** Resume a context that started suspended (call from a user gesture). */
  async resume(): Promise<void> {
    if (this.ctx?.state === "suspended") await this.ctx.resume().catch(() => {});
  }

  /** Stop/resume *sending* audio (the half-duplex degrade). The socket stays
   *  open; we just don't feed it. Default is ungated (full-duplex). */
  setGated(gated: boolean): void {
    if (this.gated === gated) return;
    this.gated = gated;
    log(gated ? "streaming: muted (not sending)" : "streaming: live");
  }

  // ── WebSocket lifecycle (with bounded reconnect) ──────────────────────────

  private connect(): void {
    if (this.closed) return;
    let ws: WebSocket;
    try {
      ws = new WebSocket(this.opts.url);
    } catch (e) {
      this.scheduleReconnect();
      log("streaming: ws construction failed", e);
      return;
    }
    this.ws = ws;
    ws.onopen = () => {
      this.reconnects = 0;
      log("streaming: socket open");
    };
    ws.onmessage = (e) => {
      if (typeof e.data === "string") this.onMessage(e.data);
    };
    ws.onerror = () => {
      // onclose follows; reconnect logic lives there.
    };
    ws.onclose = () => {
      if (this.ws === ws) this.ws = undefined;
      if (this.closed) return;
      this.scheduleReconnect();
    };
  }

  private scheduleReconnect(): void {
    if (this.closed) return;
    if (this.reconnects >= STREAM_MAX_RECONNECTS) {
      this.opts.onError?.(new Error("streaming socket unavailable (gave up reconnecting)"));
      return;
    }
    this.reconnects++;
    const delay = Math.min(5000, 250 * 2 ** (this.reconnects - 1));
    log(`streaming: socket closed — reconnecting in ${delay}ms (#${this.reconnects})`);
    this.reconnectTimer = setTimeout(() => this.connect(), delay);
  }

  private onMessage(raw: string): void {
    // A message can still be delivered after we've torn down (the engine flushes
    // a final frame on "end" before closing); ignore it so a half-spoken
    // utterance at mute-time can't fire a stray turn.
    if (this.closed) return;
    let frame: StreamFrame;
    try {
      frame = JSON.parse(raw) as StreamFrame;
    } catch {
      return;
    }
    if (frame.error) {
      // A fatal engine error (e.g. onnxruntime missing); the engine closes the
      // socket after this, so let reconnect-then-give-up surface it.
      this.opts.onStatus?.(String(frame.error));
      log("streaming: engine error", frame.error);
      return;
    }
    if (typeof frame.status === "string") this.opts.onStatus?.(frame.status);
    for (const seg of frame.segments ?? []) {
      const text = seg.text ?? "";
      const segId = seg.seg_id ?? 0;
      if (seg.partial === true) {
        if (segId !== this.currentSegId) {
          this.currentSegId = segId;
          this.opts.onSpeechStart?.();
        }
        this.opts.onInterim(text, segId);
      } else {
        // Finalized utterance.
        if (segId === this.currentSegId) this.currentSegId = -1;
        this.opts.onFinal(text, segId);
      }
    }
    // `final: true` is the *session* ending. We never ask for that (we stream
    // continuously), so if it arrives the engine tore the session down — let
    // the socket close and reconnect.
  }

  // ── Capture → resample → send ─────────────────────────────────────────────

  private onFrame(frame: Float32Array): void {
    if (this.closed) return;
    const now = performance.now();
    if (now - this.lastLevelLog > 2000) {
      this.lastLevelLog = now;
      this.opts.onLevel?.(computeRms(frame));
    }
    const pcm = this.resampleToPcm16le(frame);
    if (this.gated || !pcm) return;
    const ws = this.ws;
    if (ws && ws.readyState === WebSocket.OPEN) {
      try {
        ws.send(pcm);
      } catch {
        /* socket raced to closed; reconnect will re-establish */
      }
    }
  }

  /** Resample one capture-rate float frame to 16 kHz and pack it as
   *  little-endian 16-bit PCM (what the engine decodes with `i16::from_le_bytes`).
   *  Returns null when the frame produced no output samples. */
  private resampleToPcm16le(frame: Float32Array): ArrayBuffer | null {
    const ratio = this.sampleRate / STREAM_SAMPLE_RATE;
    const tail = this.resample.tail;
    const buf = new Float32Array(tail.length + frame.length);
    buf.set(tail, 0);
    buf.set(frame, tail.length);

    const out: number[] = [];
    let pos = this.resample.pos;
    while (Math.floor(pos) + 1 < buf.length) {
      const i = Math.floor(pos);
      const frac = pos - i;
      let s = buf[i] * (1 - frac) + buf[i + 1] * frac;
      if (s > 1) s = 1;
      else if (s < -1) s = -1;
      out.push(s < 0 ? s * 0x8000 : s * 0x7fff);
      pos += ratio;
    }
    const keep = Math.min(Math.floor(pos), buf.length);
    this.resample.tail = buf.slice(keep);
    this.resample.pos = pos - keep;

    if (out.length === 0) return null;
    const view = new DataView(new ArrayBuffer(out.length * 2));
    for (let k = 0; k < out.length; k++) view.setInt16(k * 2, out[k] | 0, true);
    return view.buffer;
  }
}
