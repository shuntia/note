import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

const api = process.env.NOTE_API ?? 'http://127.0.0.1:3271'

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      // The server takes sockets only from its own origin, so the dev page's is swapped for the server's.
      '/api': {
        target: api,
        ws: true,
        configure: (proxy) => proxy.on('proxyReqWs', (req) => req.setHeader('origin', new URL(api).origin)),
      },
    },
  },
})
