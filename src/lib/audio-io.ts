// Audio I/O for the shell — Myo's voice out *and* her ears.
//
// Voice OUT: play the brain's synthesized speech, or fall back to the
// browser's speech engine when no TTS provider is configured. One `Voice`
// instance backs the whole app so barge-in can stop whatever is playing.
//
// Voice IN: Myo is a character who's always listening. `Listener` opens the
// mic once and keeps it open, watching for utterances with a lightweight
// energy VAD (speech onset → trailing-silence endpoint). Each finished
// utterance is handed back as a 16-bit WAV the shell forwards to MyOwnLLM's
// transcription route. Capture lives in the WebView on purpose: the browser's
// echo-cancellation then sees the same audio context as the TTS playback, and
// nothing audible ever leaves the device. The audio is never persisted — it's
// transcribed and dropped (Myo's privacy default).

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
