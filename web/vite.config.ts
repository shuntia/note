import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      '/api': { target: process.env.NOTE_API ?? 'http://127.0.0.1:3271', ws: true },
    },
  },
})
