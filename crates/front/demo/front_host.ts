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
  r9p_front_complete_request: {
    parameters: ["pointer", "buffer", "usize", "u64", "buffer", "usize"],
    result: "i32",
  },
  r9p_front_stop: { parameters: ["pointer"], result: "i32" },
});

const text = new TextEncoder();
const str = (value: string): [Uint8Array, number] => {
  const bytes = text.encode(value);
  return [bytes, bytes.length];
};

if (lib.symbols.r9p_front_abi_version() !== 4) {
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
    const buf = new Uint8Array(len);
    lib.symbols.r9p_front_request_copy(front, requestId, buf, len);
    console.log(`query ${requestId}: ${new TextDecoder().decode(buf)}`);
    const [result, resultLen] = str('#M("hits" ())');
    lib.symbols.r9p_front_complete_request(front, intake, intakeLen, requestId, result, resultLen);
  }
  tick += 1;
  const [eventsPath, eventsPathLen] = str("market/events");
  const [event, eventLen] = str(`#M("tick" ${tick})\n`);
  lib.symbols.r9p_front_append_event(front, eventsPath, eventsPathLen, event, eventLen);
}
