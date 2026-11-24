import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// porta fixa: o tauri.conf.json aponta o devUrl para ela
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  // ".." libera o locales/app.yml da raiz do repositório (as chaves de tradução)
  server: { port: 1420, strictPort: true, fs: { allow: [".."] } },
  // é um app local: um bundle só, sem aviso de tamanho
  build: { chunkSizeWarningLimit: 2000 },
});
