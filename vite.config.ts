import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// https://vitejs.dev/config/ — tuned for Tauri (fixed port, no clobbering the
// terminal, and the browser export of Svelte so production `mount()` works).
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  resolve: {
    // Force the client build of Svelte; the SSR stub throws
    // `lifecycle_function_unavailable` and leaves the WebView blank.
    conditions: ["browser", "module", "import", "default"],
  },
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**", "**/target/**"] },
  },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: {
    target: "chrome105",
    minify: !process.env.TAURI_ENV_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
  },
});
