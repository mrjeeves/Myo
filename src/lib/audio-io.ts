// Voice output for the shell: play the brain's synthesized speech, or fall back
// to the browser's speech engine when no TTS provider is configured. One
// `Voice` instance backs the whole app so barge-in can stop whatever is playing.
//
// Voice *input* (open-mic capture → VAD → ASR) is the `myo-asr` seam and lands
// with that engine; until then the shell drives turns via the text composer
// (`api.say`). `micAvailable()` lets the Presence UI reflect that honestly.

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
