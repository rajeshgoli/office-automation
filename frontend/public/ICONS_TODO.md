# App Icons TODO

The PWA manifest requires PNG icons. A base SVG has been created at `icon.svg`.

## Generate PNG icons:

### Option 1: Online converter
1. Go to https://cloudconvert.com/svg-to-png
2. Upload `icon.svg`
3. Generate 192x192 → save as `icon-192.png`
4. Generate 512x512 → save as `icon-512.png`

### Option 2: ImageMagick (if installed)
```bash
cd frontend/public
convert -density 300 -background none icon.svg -resize 192x192 icon-192.png
convert -density 300 -background none icon.svg -resize 512x512 icon-512.png
```

### Option 3: macOS Preview
1. Open icon.svg in Preview
2. File → Export → Format: PNG, Resolution: 300 DPI
3. Resize to 192x192 and 512x512 as needed

## Temporary Workaround
Until proper icons are generated, the PWA will use the SVG as fallback. iOS will show a screenshot of the page as the app icon.
