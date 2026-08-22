import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    // The app calls its own origin, so in dev the API is proxied to a local
    // backend. Production serves the built client from the backend itself.
    proxy: Object.fromEntries(
      ["/state", "/voice", "/command", "/control", "/auth", "/healthz"].map(
        (path) => [path, { target: "http://127.0.0.1:3000", changeOrigin: true }],
      ),
    ),
  },
  build: { outDir: "dist", sourcemap: true },
});
