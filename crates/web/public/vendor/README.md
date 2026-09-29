# Vendored Mermaid

`mermaid-12.0.0.min.js` is the unmodified browser bundle from
https://registry.npmjs.org/mermaid/-/mermaid-12.0.0.tgz (`package/dist/mermaid.min.js`).
SHA-256: `28fca7ae6ebc7ed7bb63bde63136a74bfef14f296a57e403657eeb8b32836073`.
MIT license is in `mermaid-LICENSE`; bundled dependency notices remain in the file.

The standalone bundle avoids a Node build toolchain and runtime CDN requests.
It is copied and hashed into each PWA release and rendered in an opaque-origin,
network-restricted iframe. Only Mermaid code fences load it. Keep this version
and the URL in `rich-messages.js` in sync when updating.
