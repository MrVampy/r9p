export const SUPPORTED_ABI_VERSION = 20;
export const FRONT_CAP_PUSHED_NAMESPACE_METADATA = 1n << 0n;
export const FRONT_CAP_REQUEST_CONTEXT_V2 = 1n << 1n;
export const FRONT_CAP_SYNTHETIC_READ_RELAY = 1n << 2n;
export const FRONT_CAP_NATIVE_CLIENT_MUTATIONS = 1n << 3n;
export const FRONT_CAP_ATOMIC_CREATE_WRITE = 1n << 4n;
export const FRONT_CAP_NAMESPACE_MUTATION_RELAYS = 1n << 5n;
export const FRONT_CAP_RESOLVED_NAMESPACE_CLIENT = 1n << 6n;
export const FRONT_CAP_AUTHENTICATED_SERVE = 1n << 7n;
export const REQUIRED_FRONT_CAPABILITIES =
  FRONT_CAP_PUSHED_NAMESPACE_METADATA |
  FRONT_CAP_REQUEST_CONTEXT_V2 |
  FRONT_CAP_SYNTHETIC_READ_RELAY |
  FRONT_CAP_NATIVE_CLIENT_MUTATIONS |
  FRONT_CAP_ATOMIC_CREATE_WRITE |
  FRONT_CAP_NAMESPACE_MUTATION_RELAYS |
  FRONT_CAP_RESOLVED_NAMESPACE_CLIENT |
  FRONT_CAP_AUTHENTICATED_SERVE;

export { renderExportDescriptor } from "./export_descriptor.ts";
export type { ExportDescriptorOptions } from "./export_descriptor.ts";
import { parseRequestContext } from "./request_context.ts";
import type { RequestContext } from "./request_context.ts";
export type { RequestContext } from "./request_context.ts";

const SYMBOLS = {
  r9p_front_abi_version: { parameters: [], result: "u32" },
  r9p_front_capabilities: { parameters: [], result: "u64" },
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
  r9p_front_serve_tcp: {
    parameters: ["pointer", "buffer", "usize", "buffer"],
    result: "i32",
  },
  r9p_front_serve_tcp_authenticated: {
    parameters: [
      "pointer",
      "buffer",
      "usize",
      "buffer",
      "usize",
      "buffer",
    ],
    result: "i32",
  },
  r9p_front_register_intake: {
    parameters: ["pointer", "buffer", "usize"],
    result: "i32",
  },
  r9p_front_register_rpc: {
    parameters: ["pointer", "buffer", "usize"],
    result: "i32",
  },
  r9p_front_register_read_relay: {
    parameters: ["pointer", "buffer", "usize"],
    result: "i32",
  },
  r9p_front_register_write_relay: {
    parameters: ["pointer", "buffer", "usize"],
    result: "i32",
  },
  r9p_front_register_remove_relay: {
    parameters: ["pointer", "buffer", "usize"],
    result: "i32",
  },
  r9p_front_register_wstat_relay: {
    parameters: ["pointer", "buffer", "usize"],
    result: "i32",
  },
  r9p_front_register_log: {
    parameters: ["pointer", "buffer", "usize"],
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
  r9p_front_request_context_copy: {
    parameters: ["pointer", "u64", "buffer", "usize"],
    result: "isize",
  },
  r9p_front_complete_request: {
    parameters: ["pointer", "buffer", "usize", "u64", "buffer", "usize"],
    result: "i32",
  },
  r9p_front_reject_request: {
    parameters: ["pointer", "buffer", "usize", "u64", "buffer", "usize"],
    result: "i32",
  },
  r9p_front_complete_write: {
    parameters: ["pointer", "buffer", "usize", "u64", "u32"],
    result: "i32",
  },
  r9p_front_reject_write: {
    parameters: ["pointer", "buffer", "usize", "u64", "buffer", "usize"],
    result: "i32",
  },
  r9p_front_complete_remove: {
    parameters: ["pointer", "buffer", "usize", "u64"],
    result: "i32",
  },
  r9p_front_reject_remove: {
    parameters: ["pointer", "buffer", "usize", "u64", "buffer", "usize"],
    result: "i32",
  },
  r9p_front_complete_wstat: {
    parameters: ["pointer", "buffer", "usize", "u64"],
    result: "i32",
  },
  r9p_front_reject_wstat: {
    parameters: ["pointer", "buffer", "usize", "u64", "buffer", "usize"],
    result: "i32",
  },
  r9p_front_client_rpc: {
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
      "u32",
      "buffer",
      "usize",
      "buffer",
    ],
    result: "i32",
  },
  r9p_front_client_read: {
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
      "u32",
      "buffer",
      "usize",
      "buffer",
    ],
    result: "i32",
  },
  r9p_front_client_resolved_rpc: {
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
      "u32",
      "u64",
      "buffer",
      "usize",
      "buffer",
    ],
    result: "i32",
  },
  r9p_front_client_resolved_read: {
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
      "u32",
      "u64",
      "buffer",
      "usize",
      "buffer",
    ],
    result: "i32",
  },
  r9p_front_client_create_at: {
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
      "u32",
      "u8",
      "u32",
      "buffer",
      "buffer",
      "buffer",
    ],
    result: "i32",
  },
  r9p_front_client_create_write_at: {
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
      "u32",
      "u8",
      "u64",
      "buffer",
      "usize",
      "u32",
      "buffer",
    ],
    result: "i32",
  },
  r9p_front_client_write_file: {
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
      "u32",
      "buffer",
    ],
    result: "i32",
  },
  r9p_front_client_remove: {
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
      "u32",
    ],
    result: "i32",
  },
  r9p_front_last_error: {
    parameters: ["pointer", "buffer", "usize"],
    result: "isize",
  },
  r9p_front_stop: { parameters: ["pointer"], result: "i32" },
} as const;

export interface TransitionSink {
  set(path: string, record: unknown): void;
  appendEvent(path: string, record: unknown): void;
}

export interface IntakeRequest {
  requestId: bigint;
  prefix: string;
  bytes: Uint8Array;
  context: RequestContext;
}

export interface ClientRpcOptions {
  endpointBind: string;
  uname: string;
  aname: string;
  path: string;
  request: string;
  msize?: number;
  responseCapacity?: number;
}

export interface ClientReadOptions {
  endpointBind: string;
  uname: string;
  aname: string;
  path: string;
  msize?: number;
  responseCapacity?: number;
}

export interface ResolvedClientCoordinates {
  resolverBind: string;
  resolverUname: string;
  resolverAname: string;
  resolverAuthConfig?: string;
  authorityBoundary?: string;
  serviceAuthConfig?: string;
  msize?: number;
  timeoutMs?: bigint;
  responseCapacity?: number;
}

export interface ResolvedClientRpcOptions extends ResolvedClientCoordinates {
  path: string;
  request: string;
}

export interface ResolvedClientReadOptions extends ResolvedClientCoordinates {
  path: string;
}

export interface ClientCreateAtOptions {
  endpointBind: string;
  uname: string;
  aname: string;
  parent: string;
  name: string;
  perm: number;
  mode: number;
  msize?: number;
}

export interface ClientCreateResult {
  qidType: number;
  qidVersion: number;
  qidPath: bigint;
}

export interface ClientCreateWriteAtOptions extends ClientCreateAtOptions {
  offset?: bigint;
  data: string | Uint8Array;
}

export interface ClientWriteFileOptions {
  endpointBind: string;
  uname: string;
  aname: string;
  path: string;
  data: string | Uint8Array;
  msize?: number;
}

export interface ClientRemoveOptions {
  endpointBind: string;
  uname: string;
  aname: string;
  path: string;
  msize?: number;
}

const encoder = new TextEncoder();
const decoder = new TextDecoder();

function bytes(value: string): [Uint8Array<ArrayBuffer>, bigint] {
  const encoded = encoder.encode(value);
  const backed = new Uint8Array(new ArrayBuffer(encoded.length));
  backed.set(encoded);
  return [backed, BigInt(backed.length)];
}

function inputBytes(
  value: string | Uint8Array,
): [Uint8Array<ArrayBuffer>, bigint] {
  if (typeof value === "string") return bytes(value);
  const backed = new Uint8Array(new ArrayBuffer(value.length));
  backed.set(value);
  return [backed, BigInt(backed.length)];
}

export class FrontHost implements TransitionSink {
  private closed = false;

  private constructor(
    private readonly library: Deno.DynamicLibrary<typeof SYMBOLS>,
    private readonly handle: NonNullable<Deno.PointerValue>,
  ) {}

  static open(libraryPath: string): FrontHost {
    const library = Deno.dlopen(libraryPath, SYMBOLS);
    const version = library.symbols.r9p_front_abi_version();
    if (version !== SUPPORTED_ABI_VERSION) {
      library.close();
      throw new Error(
        `front ABI version mismatch: library has ${version}, host requires ${SUPPORTED_ABI_VERSION}`,
      );
    }
    const capabilities = library.symbols.r9p_front_capabilities();
    const missingCapabilities = REQUIRED_FRONT_CAPABILITIES & ~capabilities;
    if (missingCapabilities !== 0n) {
      library.close();
      throw new Error(
        `front capability mismatch: library has 0x${capabilities.toString(16)}, missing 0x${missingCapabilities.toString(16)}`,
      );
    }
    const handle = library.symbols.r9p_front_new();
    if (handle === null) {
      library.close();
      throw new Error("front handle allocation failed");
    }
    return new FrontHost(library, handle);
  }

  serve(bind: string, sessionAuthConfig?: string): number {
    this.assertOpen();
    const [bindBytes, bindLen] = bytes(bind);
    const portOut = new Uint8Array(2);
    const status = sessionAuthConfig === undefined
      ? this.library.symbols.r9p_front_serve_tcp(
        this.handle,
        bindBytes,
        bindLen,
        portOut,
      )
      : (() => {
        const [authConfigBytes, authConfigLen] = bytes(sessionAuthConfig);
        return this.library.symbols.r9p_front_serve_tcp_authenticated(
          this.handle,
          bindBytes,
          bindLen,
          authConfigBytes,
          authConfigLen,
          portOut,
        );
      })();
    if (status !== 0) {
      throw new Error(
        `front serve_tcp(${bind}) failed with status ${status}: ${this.lastError()}`,
      );
    }
    return new DataView(portOut.buffer).getUint16(0, true);
  }

  clientRpc(options: ClientRpcOptions): string {
    this.assertOpen();
    const [endpoint, endpointLen] = bytes(options.endpointBind);
    const [uname, unameLen] = bytes(options.uname);
    const [aname, anameLen] = bytes(options.aname);
    const [path, pathLen] = bytes(options.path);
    const [request, requestLen] = bytes(options.request);
    const response = new Uint8Array(
      new ArrayBuffer(options.responseCapacity ?? 65_536),
    );
    const responseLenOut = new Uint8Array(new ArrayBuffer(8));
    const status = this.library.symbols.r9p_front_client_rpc(
      this.handle,
      endpoint,
      endpointLen,
      uname,
      unameLen,
      aname,
      anameLen,
      path,
      pathLen,
      request,
      requestLen,
      options.msize ?? 65_536,
      response,
      BigInt(response.length),
      responseLenOut,
    );
    const responseLen = Number(
      new DataView(responseLenOut.buffer).getBigUint64(0, true),
    );
    if (status !== 0) {
      throw new Error(
        `front client_rpc(${options.path}) failed with status ${status}: ${this.lastError()}`,
      );
    }
    if (responseLen > response.length) {
      throw new Error(
        `front client_rpc(${options.path}) response exceeded buffer: ${responseLen} > ${response.length}`,
      );
    }
    return decoder.decode(response.slice(0, responseLen));
  }

  clientRead(options: ClientReadOptions): string {
    this.assertOpen();
    const [endpoint, endpointLen] = bytes(options.endpointBind);
    const [uname, unameLen] = bytes(options.uname);
    const [aname, anameLen] = bytes(options.aname);
    const [path, pathLen] = bytes(options.path);
    const response = new Uint8Array(
      new ArrayBuffer(options.responseCapacity ?? 65_536),
    );
    const responseLenOut = new Uint8Array(new ArrayBuffer(8));
    const status = this.library.symbols.r9p_front_client_read(
      this.handle,
      endpoint,
      endpointLen,
      uname,
      unameLen,
      aname,
      anameLen,
      path,
      pathLen,
      options.msize ?? 65_536,
      response,
      BigInt(response.length),
      responseLenOut,
    );
    const responseLen = Number(
      new DataView(responseLenOut.buffer).getBigUint64(0, true),
    );
    if (status !== 0) {
      throw new Error(
        `front client_read(${options.path}) failed with status ${status}: ${this.lastError()}`,
      );
    }
    if (responseLen > response.length) {
      throw new Error(
        `front client_read(${options.path}) response exceeded buffer: ${responseLen} > ${response.length}`,
      );
    }
    return decoder.decode(response.slice(0, responseLen));
  }

  resolvedRpc(options: ResolvedClientRpcOptions): string {
    this.assertOpen();
    const [resolverBind, resolverBindLen] = bytes(options.resolverBind);
    const [resolverUname, resolverUnameLen] = bytes(options.resolverUname);
    const [resolverAname, resolverAnameLen] = bytes(options.resolverAname);
    const [resolverAuth, resolverAuthLen] = bytes(options.resolverAuthConfig ?? "");
    const [path, pathLen] = bytes(options.path);
    const [authority, authorityLen] = bytes(options.authorityBoundary ?? "");
    const [serviceAuth, serviceAuthLen] = bytes(options.serviceAuthConfig ?? "");
    const [request, requestLen] = bytes(options.request);
    const response = new Uint8Array(
      new ArrayBuffer(options.responseCapacity ?? 65_536),
    );
    const responseLenOut = new Uint8Array(new ArrayBuffer(8));
    const status = this.library.symbols.r9p_front_client_resolved_rpc(
      this.handle,
      resolverBind,
      resolverBindLen,
      resolverUname,
      resolverUnameLen,
      resolverAname,
      resolverAnameLen,
      resolverAuth,
      resolverAuthLen,
      path,
      pathLen,
      authority,
      authorityLen,
      serviceAuth,
      serviceAuthLen,
      request,
      requestLen,
      options.msize ?? 65_536,
      options.timeoutMs ?? 5_000n,
      response,
      BigInt(response.length),
      responseLenOut,
    );
    return this.decodeResolvedResponse("rpc", options.path, status, response, responseLenOut);
  }

  resolvedRead(options: ResolvedClientReadOptions): string {
    this.assertOpen();
    const [resolverBind, resolverBindLen] = bytes(options.resolverBind);
    const [resolverUname, resolverUnameLen] = bytes(options.resolverUname);
    const [resolverAname, resolverAnameLen] = bytes(options.resolverAname);
    const [resolverAuth, resolverAuthLen] = bytes(options.resolverAuthConfig ?? "");
    const [path, pathLen] = bytes(options.path);
    const [authority, authorityLen] = bytes(options.authorityBoundary ?? "");
    const [serviceAuth, serviceAuthLen] = bytes(options.serviceAuthConfig ?? "");
    const response = new Uint8Array(
      new ArrayBuffer(options.responseCapacity ?? 65_536),
    );
    const responseLenOut = new Uint8Array(new ArrayBuffer(8));
    const status = this.library.symbols.r9p_front_client_resolved_read(
      this.handle,
      resolverBind,
      resolverBindLen,
      resolverUname,
      resolverUnameLen,
      resolverAname,
      resolverAnameLen,
      resolverAuth,
      resolverAuthLen,
      path,
      pathLen,
      authority,
      authorityLen,
      serviceAuth,
      serviceAuthLen,
      options.msize ?? 65_536,
      options.timeoutMs ?? 5_000n,
      response,
      BigInt(response.length),
      responseLenOut,
    );
    return this.decodeResolvedResponse("read", options.path, status, response, responseLenOut);
  }

  private decodeResolvedResponse(
    operation: "rpc" | "read",
    path: string,
    status: number,
    response: Uint8Array<ArrayBuffer>,
    responseLenOut: Uint8Array<ArrayBuffer>,
  ): string {
    const responseLen = Number(
      new DataView(responseLenOut.buffer).getBigUint64(0, true),
    );
    if (status !== 0) {
      throw new Error(
        `front resolved_${operation}(${path}) failed with status ${status}: ${this.lastError()}`,
      );
    }
    if (responseLen > response.length) {
      throw new Error(
        `front resolved_${operation}(${path}) response exceeded buffer: ${responseLen} > ${response.length}`,
      );
    }
    return decoder.decode(response.slice(0, responseLen));
  }

  clientCreateAt(options: ClientCreateAtOptions): ClientCreateResult {
    this.assertOpen();
    const [endpoint, endpointLen] = bytes(options.endpointBind);
    const [uname, unameLen] = bytes(options.uname);
    const [aname, anameLen] = bytes(options.aname);
    const [parent, parentLen] = bytes(options.parent);
    const [name, nameLen] = bytes(options.name);
    const qidTypeOut = new Uint8Array(new ArrayBuffer(1));
    const qidVersionOut = new Uint8Array(new ArrayBuffer(4));
    const qidPathOut = new Uint8Array(new ArrayBuffer(8));
    const status = this.library.symbols.r9p_front_client_create_at(
      this.handle,
      endpoint,
      endpointLen,
      uname,
      unameLen,
      aname,
      anameLen,
      parent,
      parentLen,
      name,
      nameLen,
      options.perm,
      options.mode,
      options.msize ?? 65_536,
      qidTypeOut,
      qidVersionOut,
      qidPathOut,
    );
    if (status !== 0) {
      throw new Error(
        `front client_create_at(${options.parent}, ${options.name}) failed with status ${status}: ${this.lastError()}`,
      );
    }
    return {
      qidType: qidTypeOut[0] ?? 0,
      qidVersion: new DataView(qidVersionOut.buffer).getUint32(0, true),
      qidPath: new DataView(qidPathOut.buffer).getBigUint64(0, true),
    };
  }

  clientCreateWriteAt(options: ClientCreateWriteAtOptions): number {
    this.assertOpen();
    const [endpoint, endpointLen] = bytes(options.endpointBind);
    const [uname, unameLen] = bytes(options.uname);
    const [aname, anameLen] = bytes(options.aname);
    const [parent, parentLen] = bytes(options.parent);
    const [name, nameLen] = bytes(options.name);
    const [data, dataLen] = inputBytes(options.data);
    const countOut = new Uint8Array(new ArrayBuffer(4));
    const status = this.library.symbols.r9p_front_client_create_write_at(
      this.handle,
      endpoint,
      endpointLen,
      uname,
      unameLen,
      aname,
      anameLen,
      parent,
      parentLen,
      name,
      nameLen,
      options.perm,
      options.mode,
      options.offset ?? 0n,
      data,
      dataLen,
      options.msize ?? 65_536,
      countOut,
    );
    if (status !== 0) {
      throw new Error(
        `front client_create_write_at(${options.parent}, ${options.name}) failed with status ${status}: ${this.lastError()}`,
      );
    }
    return new DataView(countOut.buffer).getUint32(0, true);
  }

  clientWriteFile(options: ClientWriteFileOptions): number {
    this.assertOpen();
    const [endpoint, endpointLen] = bytes(options.endpointBind);
    const [uname, unameLen] = bytes(options.uname);
    const [aname, anameLen] = bytes(options.aname);
    const [path, pathLen] = bytes(options.path);
    const [data, dataLen] = inputBytes(options.data);
    const countOut = new Uint8Array(new ArrayBuffer(4));
    const status = this.library.symbols.r9p_front_client_write_file(
      this.handle,
      endpoint,
      endpointLen,
      uname,
      unameLen,
      aname,
      anameLen,
      path,
      pathLen,
      data,
      dataLen,
      options.msize ?? 65_536,
      countOut,
    );
    if (status !== 0) {
      throw new Error(
        `front client_write_file(${options.path}) failed with status ${status}: ${this.lastError()}`,
      );
    }
    return new DataView(countOut.buffer).getUint32(0, true);
  }

  clientRemove(options: ClientRemoveOptions): void {
    this.assertOpen();
    const [endpoint, endpointLen] = bytes(options.endpointBind);
    const [uname, unameLen] = bytes(options.uname);
    const [aname, anameLen] = bytes(options.aname);
    const [path, pathLen] = bytes(options.path);
    const status = this.library.symbols.r9p_front_client_remove(
      this.handle,
      endpoint,
      endpointLen,
      uname,
      unameLen,
      aname,
      anameLen,
      path,
      pathLen,
      options.msize ?? 65_536,
    );
    if (status !== 0) {
      throw new Error(
        `front client_remove(${options.path}) failed with status ${status}: ${this.lastError()}`,
      );
    }
  }

  set(path: string, record: unknown): void {
    this.assertOpen();
    const [pathBytes, pathLen] = bytes(path);
    const [body, bodyLen] = bytes(`${JSON.stringify(record, null, 2)}\n`);
    const status = this.library.symbols.r9p_front_set(
      this.handle,
      pathBytes,
      pathLen,
      body,
      bodyLen,
    );
    if (status !== 0) {
      throw new Error(`front set(${path}) failed with status ${status}`);
    }
  }

  setText(path: string, text: string): void {
    this.assertOpen();
    const [pathBytes, pathLen] = bytes(path);
    const [body, bodyLen] = bytes(`${text}\n`);
    const status = this.library.symbols.r9p_front_set(
      this.handle,
      pathBytes,
      pathLen,
      body,
      bodyLen,
    );
    if (status !== 0) {
      throw new Error(`front set(${path}) failed with status ${status}`);
    }
  }

  appendEvent(path: string, record: unknown): void {
    this.assertOpen();
    const [pathBytes, pathLen] = bytes(path);
    const [body, bodyLen] = bytes(`${JSON.stringify(record)}\n`);
    const status = this.library.symbols.r9p_front_append_event(
      this.handle,
      pathBytes,
      pathLen,
      body,
      bodyLen,
    );
    if (status !== 0) {
      throw new Error(
        `front append_event(${path}) failed with status ${status}`,
      );
    }
  }

  registerIntake(prefix: string): void {
    this.assertOpen();
    const [prefixBytes, prefixLen] = bytes(prefix);
    const status = this.library.symbols.r9p_front_register_intake(
      this.handle,
      prefixBytes,
      prefixLen,
    );
    if (status !== 0) {
      throw new Error(
        `front register_intake(${prefix}) failed with status ${status}`,
      );
    }
  }

  registerRpc(path: string): void {
    this.assertOpen();
    const [pathBytes, pathLen] = bytes(path);
    const status = this.library.symbols.r9p_front_register_rpc(
      this.handle,
      pathBytes,
      pathLen,
    );
    if (status !== 0) {
      throw new Error(
        `front register_rpc(${path}) failed with status ${status}`,
      );
    }
  }

  registerReadRelay(path: string): void {
    this.assertOpen();
    const [pathBytes, pathLen] = bytes(path);
    const status = this.library.symbols.r9p_front_register_read_relay(
      this.handle,
      pathBytes,
      pathLen,
    );
    if (status !== 0) {
      throw new Error(
        `front register_read_relay(${path}) failed with status ${status}`,
      );
    }
  }

  registerWriteRelay(path: string): void {
    this.assertOpen();
    const [pathBytes, pathLen] = bytes(path);
    const status = this.library.symbols.r9p_front_register_write_relay(
      this.handle,
      pathBytes,
      pathLen,
    );
    if (status !== 0) {
      throw new Error(
        `front register_write_relay(${path}) failed with status ${status}`,
      );
    }
  }

  registerRemoveRelay(path: string): void {
    this.assertOpen();
    const [pathBytes, pathLen] = bytes(path);
    const status = this.library.symbols.r9p_front_register_remove_relay(
      this.handle,
      pathBytes,
      pathLen,
    );
    if (status !== 0) {
      throw new Error(
        `front register_remove_relay(${path}) failed with status ${status}`,
      );
    }
  }

  registerWstatRelay(path: string): void {
    this.assertOpen();
    const [pathBytes, pathLen] = bytes(path);
    const status = this.library.symbols.r9p_front_register_wstat_relay(
      this.handle,
      pathBytes,
      pathLen,
    );
    if (status !== 0) {
      throw new Error(
        `front register_wstat_relay(${path}) failed with status ${status}`,
      );
    }
  }

  registerLog(path: string): void {
    this.assertOpen();
    const [pathBytes, pathLen] = bytes(path);
    const status = this.library.symbols.r9p_front_register_log(
      this.handle,
      pathBytes,
      pathLen,
    );
    if (status !== 0) {
      throw new Error(
        `front register_log(${path}) failed with status ${status}`,
      );
    }
  }

  async nextRequest(timeoutMs: number): Promise<IntakeRequest | null> {
    this.assertOpen();
    const idOut = new Uint8Array(8);
    const lenOut = new Uint8Array(8);
    const status = await this.library.symbols.r9p_front_next_request(
      this.handle,
      BigInt(timeoutMs),
      idOut,
      lenOut,
    );
    if (status === 1) {
      return null;
    }
    if (status !== 0) {
      throw new Error(`front next_request failed with status ${status}`);
    }
    const requestId = new DataView(idOut.buffer).getBigUint64(0, true);
    const len = Number(new DataView(lenOut.buffer).getBigUint64(0, true));
    const prefixLen = Number(this.library.symbols.r9p_front_request_prefix_copy(
      this.handle,
      requestId,
      new Uint8Array(new ArrayBuffer(0)),
      0n,
    ));
    if (prefixLen < 0) {
      throw new Error(`front request_prefix_copy length returned ${prefixLen}`);
    }
    const prefixBuf = new Uint8Array(new ArrayBuffer(prefixLen));
    const prefixCopied = this.library.symbols.r9p_front_request_prefix_copy(
      this.handle,
      requestId,
      prefixBuf,
      BigInt(prefixLen),
    );
    if (Number(prefixCopied) !== prefixLen) {
      throw new Error(
        `front request_prefix_copy returned ${prefixCopied}, expected ${prefixLen}`,
      );
    }
    const contextLen = Number(
      this.library.symbols.r9p_front_request_context_copy(
        this.handle,
        requestId,
        new Uint8Array(new ArrayBuffer(0)),
        0n,
      ),
    );
    if (contextLen < 0) {
      throw new Error(
        `front request_context_copy length returned ${contextLen}`,
      );
    }
    const contextBuf = new Uint8Array(new ArrayBuffer(contextLen));
    const contextCopied = this.library.symbols.r9p_front_request_context_copy(
      this.handle,
      requestId,
      contextBuf,
      BigInt(contextLen),
    );
    if (Number(contextCopied) !== contextLen) {
      throw new Error(
        `front request_context_copy returned ${contextCopied}, expected ${contextLen}`,
      );
    }
    const context = parseRequestContext(decoder.decode(contextBuf));
    const buf = new Uint8Array(new ArrayBuffer(len));
    const copied = this.library.symbols.r9p_front_request_copy(
      this.handle,
      requestId,
      buf,
      BigInt(len),
    );
    if (Number(copied) !== len) {
      throw new Error(`front request_copy returned ${copied}, expected ${len}`);
    }
    return {
      requestId,
      prefix: decoder.decode(prefixBuf),
      bytes: buf,
      context,
    };
  }

  completeRequest(prefix: string, requestId: bigint, result: string): void {
    this.assertOpen();
    const [prefixBytes, prefixLen] = bytes(prefix);
    const [body, bodyLen] = bytes(result);
    const status = this.library.symbols.r9p_front_complete_request(
      this.handle,
      prefixBytes,
      prefixLen,
      requestId,
      body,
      bodyLen,
    );
    if (status !== 0) {
      throw new Error(
        `front complete_request(${prefix}) failed with status ${status}`,
      );
    }
  }

  rejectRequest(prefix: string, requestId: bigint, message: string): void {
    this.assertOpen();
    const [prefixBytes, prefixLen] = bytes(prefix);
    const [messageBytes, messageLen] = bytes(message);
    const status = this.library.symbols.r9p_front_reject_request(
      this.handle,
      prefixBytes,
      prefixLen,
      requestId,
      messageBytes,
      messageLen,
    );
    if (status !== 0) {
      throw new Error(
        `front reject_request(${prefix}) failed with status ${status}`,
      );
    }
  }

  completeWrite(prefix: string, requestId: bigint, count: number): void {
    this.assertOpen();
    const [prefixBytes, prefixLen] = bytes(prefix);
    const status = this.library.symbols.r9p_front_complete_write(
      this.handle,
      prefixBytes,
      prefixLen,
      requestId,
      count,
    );
    if (status !== 0) {
      throw new Error(
        `front complete_write(${prefix}) failed with status ${status}`,
      );
    }
  }

  rejectWrite(prefix: string, requestId: bigint, message: string): void {
    this.assertOpen();
    const [prefixBytes, prefixLen] = bytes(prefix);
    const [messageBytes, messageLen] = bytes(message);
    const status = this.library.symbols.r9p_front_reject_write(
      this.handle,
      prefixBytes,
      prefixLen,
      requestId,
      messageBytes,
      messageLen,
    );
    if (status !== 0) {
      throw new Error(
        `front reject_write(${prefix}) failed with status ${status}`,
      );
    }
  }

  completeRemove(prefix: string, requestId: bigint): void {
    this.assertOpen();
    const [prefixBytes, prefixLen] = bytes(prefix);
    const status = this.library.symbols.r9p_front_complete_remove(
      this.handle,
      prefixBytes,
      prefixLen,
      requestId,
    );
    if (status !== 0) {
      throw new Error(
        `front complete_remove(${prefix}) failed with status ${status}`,
      );
    }
  }

  rejectRemove(prefix: string, requestId: bigint, message: string): void {
    this.assertOpen();
    const [prefixBytes, prefixLen] = bytes(prefix);
    const [messageBytes, messageLen] = bytes(message);
    const status = this.library.symbols.r9p_front_reject_remove(
      this.handle,
      prefixBytes,
      prefixLen,
      requestId,
      messageBytes,
      messageLen,
    );
    if (status !== 0) {
      throw new Error(
        `front reject_remove(${prefix}) failed with status ${status}`,
      );
    }
  }

  completeWstat(prefix: string, requestId: bigint): void {
    this.assertOpen();
    const [prefixBytes, prefixLen] = bytes(prefix);
    const status = this.library.symbols.r9p_front_complete_wstat(
      this.handle,
      prefixBytes,
      prefixLen,
      requestId,
    );
    if (status !== 0) {
      throw new Error(
        `front complete_wstat(${prefix}) failed with status ${status}`,
      );
    }
  }

  rejectWstat(prefix: string, requestId: bigint, message: string): void {
    this.assertOpen();
    const [prefixBytes, prefixLen] = bytes(prefix);
    const [messageBytes, messageLen] = bytes(message);
    const status = this.library.symbols.r9p_front_reject_wstat(
      this.handle,
      prefixBytes,
      prefixLen,
      requestId,
      messageBytes,
      messageLen,
    );
    if (status !== 0) {
      throw new Error(
        `front reject_wstat(${prefix}) failed with status ${status}`,
      );
    }
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.library.symbols.r9p_front_stop(this.handle);
    this.library.symbols.r9p_front_free(this.handle);
    this.library.close();
  }

  private assertOpen(): void {
    if (this.closed) throw new Error("front host is closed");
  }

  private lastError(): string {
    const empty = new Uint8Array(new ArrayBuffer(0));
    const len = Number(
      this.library.symbols.r9p_front_last_error(this.handle, empty, 0n),
    );
    if (len <= 0) {
      return "no ABI error detail";
    }
    const buf = new Uint8Array(new ArrayBuffer(len));
    const copied = Number(
      this.library.symbols.r9p_front_last_error(
        this.handle,
        buf,
        BigInt(buf.length),
      ),
    );
    if (copied < 0) {
      return `last_error failed with status ${copied}`;
    }
    return decoder.decode(buf.slice(0, copied));
  }
}

export function resolveFrontLibrary(
  flagValue: string | undefined,
): string | { error: string } {
  const fromEnv = Deno.env.get("R9P_FRONT_LIB");
  const path = flagValue ?? fromEnv;
  if (path === undefined || path === "") {
    return {
      error:
        "front library path required: pass --front-lib <path> or set R9P_FRONT_LIB",
    };
  }
  return path;
}
