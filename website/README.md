# MCPortal website

Landing page published at <https://inseefrlab.github.io/MCPortal/>. Built with Vite, React,
Tailwind CSS and Motion; the detailed documentation stays in the repository's Markdown files
(`README.md`, `ARCHITECTURE.md`, `CONTRIBUTING.md`), which the page links to.

```bash
npm install
npm run dev      # http://localhost:5173
npm run build    # production build in dist/
npm run lint
```

| File | Content |
| --- | --- |
| `src/index.css` | palette (blue and orange portals from the logo) and fonts |
| `src/links.ts` | every outbound link |
| `src/components/Hero.tsx` | title, tagline and calls to action |
| `src/components/PortalFlow.tsx` | animated diagram: client → blue portal → orange portal → servers |
| `src/components/Features.tsx` | feature list in the footer |

Pushing changes under `website/` deploys the site through
`.github/workflows/website-pages.yml`. GitHub Pages must be enabled once with
**Settings → Pages → Source: GitHub Actions**.
