const lib = Deno.dlopen("../../../target/debug/libfront.so", {
  r9p_front_abi_version: { parameters: [], result: "u32" },
  r9p_front_new: { parameters: [], result: "pointer" },
  r9p_front_free: { parameters: ["pointer"], result: "void" },
  r9p_front_set: {
    parameters: ["pointer", "buffer", "usize", "buffer", "usize"],
    result: "i32",
  },
  r9p_front_append_event: {
    parameters: ["pointer", "buffer", "usize", "buffer", "usize"],
    result: "i32",
  },
  r9p_front_register_intake: {
    parameters: ["pointer", "buffer", "usize"],
    result: "i32",
  },
  r9p_front_serve_tcp: {
    parameters: ["pointer", "buffer", "usize", "buffer"],
    result: "i32",
  },
  r9p_front_next_request: {
    parameters: ["pointer", "u64", "buffer", "buffer"],
    result: "i32",
    nonblocking: true,
  },
  r9p_front_request_copy: {
    parameters: ["pointer", "u64", "buffer", "usize"],
    result: "isize",
  },
  r9p_front_request_prefix_copy: {
    parameters: ["pointer", "u64", "buffer", "usize"],
    result: "isize",
  },
  r9p_front_complete_request: {
    parameters: ["pointer", "buffer", "usize", "u64", "buffer", "usize"],
    result: "i32",
  },
  r9p_front_publish_r9p_export: {
    parameters: [
      "pointer",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "u32",
      "u32",
      "buffer",
      "usize",
      "buffer",
      "usize",
    ],
    result: "i32",
  },
  r9p_front_maintain_r9p_export: {
    parameters: [
      "pointer",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "u32",
      "u32",
      "u32",
      "buffer",
      "usize",
      "buffer",
      "usize",
    ],
    result: "i32",
  },
  r9p_front_reconcile_r9p_exports: { parameters: ["pointer"], result: "i32" },
  r9p_front_last_error: { parameters: ["pointer", "buffer", "usize"], result: "isize" },
  r9p_front_stop: { parameters: ["pointer"], result: "i32" },
});

const text = new TextEncoder();
const str = (value: string): [Uint8Array, number] => {
  const bytes = text.encode(value);
  return [bytes, bytes.length];
};

if (lib.symbols.r9p_front_abi_version() !== 10) {
  throw new Error("abi version mismatch");
}
const front = lib.symbols.r9p_front_new();
const [statusPath, statusPathLen] = str("market/status");
const [statusBody, statusBodyLen] = str('#M("state" \'open)');
lib.symbols.r9p_front_set(front, statusPath, statusPathLen, statusBody, statusBodyLen);
const [intake, intakeLen] = str("queries");
lib.symbols.r9p_front_register_intake(front, intake, intakeLen);
const [bind, bindLen] = str("127.0.0.1:0");
const portOut = new Uint8Array(2);
lib.symbols.r9p_front_serve_tcp(front, bind, bindLen, portOut);
const port = new DataView(portOut.buffer).getUint16(0, true);
console.log(`front serving 9P on 127.0.0.1:${port}`);
console.log(`try: r9p -u demo -A / --bind 127.0.0.1:${port} cat /market/status`);

const idOut = new Uint8Array(8);
const lenOut = new Uint8Array(8);
let tick = 0;
while (true) {
  const status = await lib.symbols.r9p_front_next_request(front, 1000n, idOut, lenOut);
  if (status === 0) {
    const requestId = new DataView(idOut.buffer).getBigUint64(0, true);
    const len = Number(new DataView(lenOut.buffer).getBigUint64(0, true));
    const prefixLen = Number(
      lib.symbols.r9p_front_request_prefix_copy(front, requestId, new Uint8Array(0), 0),
    );
    const prefixBuf = new Uint8Array(prefixLen);
    lib.symbols.r9p_front_request_prefix_copy(front, requestId, prefixBuf, prefixBuf.length);
    const buf = new Uint8Array(len);
    lib.symbols.r9p_front_request_copy(front, requestId, buf, len);
    const prefix = new TextDecoder().decode(prefixBuf);
    console.log(`${prefix} ${requestId}: ${new TextDecoder().decode(buf)}`);
    const [result, resultLen] = str('#M("hits" ())');
    lib.symbols.r9p_front_complete_request(front, prefixBuf, prefixBuf.length, requestId, result, resultLen);
  }
  tick += 1;
  const [eventsPath, eventsPathLen] = str("market/events");
  const [event, eventLen] = str(`#M("tick" ${tick})\n`);
  lib.symbols.r9p_front_append_event(front, eventsPath, eventsPathLen, event, eventLen);
}
