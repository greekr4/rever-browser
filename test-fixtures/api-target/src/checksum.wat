(module
  ;; ground truth: export "checksum" — sums two i32s (stand-in for a signer)
  (func $checksum (export "checksum") (param $a i32) (param $b i32) (result i32)
    local.get $a
    local.get $b
    i32.add))
