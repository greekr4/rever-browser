// External module that loads the WASM integrity signer. Kept as its own
// captured Script so `wasm_xref` can bridge the JS call site -> the wasm export.
export async function loadSigner() {
  const res = await fetch('/checkout.wasm')
  const { instance } = await WebAssembly.instantiate(await res.arrayBuffer(), {})
  // call site references export "priceToken" (a, b) -> i32
  return (total, nonce) => instance.exports.priceToken(total | 0, nonce | 0) >>> 0
}
