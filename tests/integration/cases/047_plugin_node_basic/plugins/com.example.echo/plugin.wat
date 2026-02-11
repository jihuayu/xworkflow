(module
  (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 1024))
  (func (export "alloc") (param $size i32) (result i32)
    (local $addr i32)
    global.get $heap
    local.set $addr
    global.get $heap
    local.get $size
    i32.add
    global.set $heap
    local.get $addr)
  (func (export "dealloc") (param i32 i32))
  (data (i32.const 0) "{\"echo\":\"ok\"}")
  (data (i32.const 32) "\00\00\00\00\0d\00\00\00")
  (func (export "handle_echo") (param i32 i32) (result i32)
    (i32.const 32))
)
