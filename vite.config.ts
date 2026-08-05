import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// papr-server default listen port (PORT=8080 in crates/papr-server/.env.example).
const apiTarget = process.env.PAPR_API_PROXY || "http://localhost:8080";

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  define: {
    __APP_VERSION__: JSON.stringify(process.env.npm_package_version || "0.15.0"),
  },
  server: {
    port: 5173,
    proxy: {
      "/api": {
        target: apiTarget,
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: "dist",
  },
});
