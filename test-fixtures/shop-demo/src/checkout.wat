;; reverzon checkout integrity signer — compiled to checkout.wasm.
;; A reverser must recover this algorithm from the shipped .wasm (no source
;; ships), which is exactly what `wasm_decompile` / `wasm_info` surface.
;;
;;   priceToken(total, nonce) = folded rolling hash, masked to 24 bits
;;   The server recomputes the same value; a tampered `total` fails the check.
(module
  (memory (export "mem") 1)
  ;; secret salt baked into the module (only visible by decompiling the wasm)
  (global $SALT i32 (i32.const 0x5f3759))   ;; 6238041

  (func $priceToken (export "priceToken") (param $total i32) (param $nonce i32) (result i32)
    (local $h i32)
    ;; h = SALT ^ (total * 2654435761)   (Knuth multiplicative hash)
    (local.set $h
      (i32.xor (global.get $SALT)
               (i32.mul (local.get $total) (i32.const -1640531527))))
    ;; h = (h << 13) | (h >>> 19)   — rotate-left 13
    (local.set $h
      (i32.or (i32.shl (local.get $h) (i32.const 13))
              (i32.shr_u (local.get $h) (i32.const 19))))
    ;; h = h + nonce * 40503
    (local.set $h
      (i32.add (local.get $h) (i32.mul (local.get $nonce) (i32.const 40503))))
    ;; return h & 0x00ffffff   (24-bit token)
    (i32.and (local.get $h) (i32.const 0x00ffffff)))
)
