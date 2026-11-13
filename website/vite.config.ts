import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// GITHUB_PAGES=true (set in CI) serves the site under https://inseefrlab.github.io/MCPortal/
export default defineConfig({
  base: process.env.GITHUB_PAGES ? '/MCPortal/' : '/',
  plugins: [react(), tailwindcss()],
})
