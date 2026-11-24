import { defineConfig } from "vite";

// Tauri expects a fixed port and no auto-opened browser.
export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true, host: "127.0.0.1" },
  build: {
    target: "es2022",
    outDir: "dist",
    emptyOutDir: true,
    rollupOptions: {
      input: {
        main: "index.html",
        player: "player.html",
      },
    },
  },
});
