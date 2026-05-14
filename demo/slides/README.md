# Slide deck

Marp source for the demo. Authored in markdown, exports to PPTX (or PDF or HTML).

## Preview locally

```bash
# Live-reload preview in browser
npx -y @marp-team/marp-cli --preview sonda-demo.md

# Or render to HTML
npx -y @marp-team/marp-cli sonda-demo.md -o sonda-demo.html
```

## Export to PPTX (for Google Slides)

```bash
npx -y @marp-team/marp-cli sonda-demo.md -o sonda-demo.pptx
```

Then in Google Drive → **New** → **File upload** → pick `sonda-demo.pptx`.
Open it → **File** → **Open with** → **Google Slides**.

Polish from there: theme, images, animations, presenter notes. The Marp markdown stays in git as the source of truth; the Google Slides version is the shareable / presenter copy.

## Export to PDF (backup for the demo)

```bash
npx -y @marp-team/marp-cli sonda-demo.md --pdf -o sonda-demo.pdf
```

Keep this on your laptop as a fallback if the live Google Slides link doesn't work on demo day.
