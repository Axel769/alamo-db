import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: {
    target: "es2021",
    // Desktop app: the bundle loads from disk, not from the network. The
    // 500 kB warning is meant for the web and here it is just noise.
    chunkSizeWarningLimit: 2000,
    rollupOptions: {
      onwarn(warning, warn) {
        // glide-data-grid ships /*#__PURE__*/ annotations in positions
        // that Rollup discards: harmless, and outside our code.
        if (warning.code === "INVALID_ANNOTATION" && warning.id?.includes("node_modules")) return;
        warn(warning);
      },
    },
  },
});
