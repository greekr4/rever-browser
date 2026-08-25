// Regenerate build/icon.{png,icns,ico} + resources/icon.png from the SVG
// masters in build/. Per-size vector renders (not downscaled bitmaps):
//   icon-macos.svg — Apple-grid margin, used for icns >=64 and Linux png
//   icon.svg       — full-bleed master, used for ico 48px+
//   icon-small.svg — filterless bold variant for 16–32px
// Run: bun scripts/gen-icons.mjs && iconutil -c icns <iconset> -o build/icon.icns
import sharp from 'sharp'
import pngToIco from 'png-to-ico'
import { mkdirSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const MACOS = 'build/icon-macos.svg'
const TIGHT = 'build/icon.svg'
const SMALL = 'build/icon-small.svg'

// SVGs declare width=1024, so density scales the vector render per size.
const render = (svg, size) =>
  sharp(svg, { density: (72 * size) / 1024 }).resize(size, size).png().toBuffer()

const iconset = join(tmpdir(), 'rever-icon.iconset')
mkdirSync(iconset, { recursive: true })

const ICONSET = [
  ['icon_16x16.png', 16, SMALL],
  ['icon_16x16@2x.png', 32, SMALL],
  ['icon_32x32.png', 32, SMALL],
  ['icon_32x32@2x.png', 64, MACOS],
  ['icon_128x128.png', 128, MACOS],
  ['icon_128x128@2x.png', 256, MACOS],
  ['icon_256x256.png', 256, MACOS],
  ['icon_256x256@2x.png', 512, MACOS],
  ['icon_512x512.png', 512, MACOS],
  ['icon_512x512@2x.png', 1024, MACOS]
]
for (const [name, size, svg] of ICONSET) {
  writeFileSync(join(iconset, name), await render(svg, size))
}

writeFileSync('build/icon.png', await render(MACOS, 1024))
writeFileSync('resources/icon.png', await render(TIGHT, 1024))

const ICO = [
  [16, SMALL], [24, SMALL], [32, SMALL],
  [48, TIGHT], [64, TIGHT], [128, TIGHT], [256, TIGHT]
]
const pngs = []
for (const [size, svg] of ICO) {
  const p = join(iconset, `ico-${size}.png`)
  writeFileSync(p, await render(svg, size))
  pngs.push(p)
}
writeFileSync('build/icon.ico', await pngToIco(pngs))

console.log('iconset dir:', iconset)
