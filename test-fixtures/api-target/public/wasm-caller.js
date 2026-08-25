// External bundle that calls the WASM signer — the realistic case wasm_xref
// bridges: a captured .js Script resource referencing a WASM export by name.
export async function loadSigner() {
  const res = await fetch('/sign.wasm')
  const { instance } = await WebAssembly.instantiate(await res.arrayBuffer(), {})
  // ground truth: this call site references the export "checksum"
  return (a, b) => instance.exports.checksum(a, b)
}
