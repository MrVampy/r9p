export const SUPPORTED_ABI_VERSIONS = new Set([13]);

const SYMBOLS = {
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
  r9p_front_serve_tcp: {
    parameters: ["pointer", "buffer", "usize", "buffer"],
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
  r9p_front_complete_request: {
    parameters: ["pointer", "buffer", "usize", "u64", "buffer", "usize"],
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
      "buffer",
      "usize",
    ],
    result: "i32",
  },
  r9p_front_reconcile_r9p_exports: { parameters: ["pointer"], result: "i32" },
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
  r9p_front_last_error: { parameters: ["pointer", "buffer", "usize"], result: "isize" },
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
}

export interface R9pExportPublicationOptions {
  vaultBind: string;
  vaultUname: string;
  vaultAname: string;
  srvName: string;
  endpointBind: string;
  exportUname: string;
  exportAname: string;
  exportedRoot: string;
  transportClass: "tcp" | "unix";
  auth: string;
  protocol: "9P2000" | "9P2000.L";
  localRootLabel: string;
  pid: number;
  msize: number;
  retryIntervalMs: number;
  serviceUnit?: string;
  hostFirewallAdmission?: string;
  namespaceMountPaths?: string[];
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

const encoder = new TextEncoder();
const decoder = new TextDecoder();

function bytes(value: string): [Uint8Array<ArrayBuffer>, bigint] {
  const encoded = encoder.encode(value);
  const backed = new Uint8Array(new ArrayBuffer(encoded.length));
  backed.set(encoded);
  return [backed, BigInt(backed.length)];
}

export class FrontHost implements TransitionSink {
  private constructor(
    private readonly library: Deno.DynamicLibrary<typeof SYMBOLS>,
    private readonly handle: NonNullable<Deno.PointerValue>,
  ) {}

  static open(libraryPath: string): FrontHost {
    const library = Deno.dlopen(libraryPath, SYMBOLS);
    const version = library.symbols.r9p_front_abi_version();
    if (!SUPPORTED_ABI_VERSIONS.has(version)) {
      library.close();
      throw new Error(
        `front ABI version mismatch: library has ${version}, host supports ${
          [...SUPPORTED_ABI_VERSIONS].join(",")
        }`,
      );
    }
    const handle = library.symbols.r9p_front_new();
    if (handle === null) {
      library.close();
      throw new Error("front handle allocation failed");
    }
    return new FrontHost(library, handle);
  }

  serve(bind: string): number {
    const [bindBytes, bindLen] = bytes(bind);
    const portOut = new Uint8Array(2);
    const status = this.library.symbols.r9p_front_serve_tcp(
      this.handle,
      bindBytes,
      bindLen,
      portOut,
    );
    if (status !== 0) {
      throw new Error(`front serve_tcp(${bind}) failed with status ${status}`);
    }
    return new DataView(portOut.buffer).getUint16(0, true);
  }

  maintainR9pExport(options: R9pExportPublicationOptions): void {
    const [vaultBind, vaultBindLen] = bytes(options.vaultBind);
    const [vaultUname, vaultUnameLen] = bytes(options.vaultUname);
    const [vaultAname, vaultAnameLen] = bytes(options.vaultAname);
    const [srvName, srvNameLen] = bytes(options.srvName);
    const [endpointBind, endpointBindLen] = bytes(options.endpointBind);
    const [exportUname, exportUnameLen] = bytes(options.exportUname);
    const [exportAname, exportAnameLen] = bytes(options.exportAname);
    const [exportedRoot, exportedRootLen] = bytes(options.exportedRoot);
    const [transportClass, transportClassLen] = bytes(options.transportClass);
    const [auth, authLen] = bytes(options.auth);
    const [protocol, protocolLen] = bytes(options.protocol);
    const [localRootLabel, localRootLabelLen] = bytes(options.localRootLabel);
    const [serviceUnit, serviceUnitLen] = bytes(options.serviceUnit ?? "");
    const [hostFirewallAdmission, hostFirewallAdmissionLen] = bytes(
      options.hostFirewallAdmission ?? "",
    );
    const [namespaceMountPaths, namespaceMountPathsLen] = bytes(
      (options.namespaceMountPaths ?? []).join(","),
    );
    const status = this.library.symbols.r9p_front_maintain_r9p_export(
      this.handle,
      vaultBind,
      vaultBindLen,
      vaultUname,
      vaultUnameLen,
      vaultAname,
      vaultAnameLen,
      srvName,
      srvNameLen,
      endpointBind,
      endpointBindLen,
      exportUname,
      exportUnameLen,
      exportAname,
      exportAnameLen,
      exportedRoot,
      exportedRootLen,
      transportClass,
      transportClassLen,
      auth,
      authLen,
      protocol,
      protocolLen,
      localRootLabel,
      localRootLabelLen,
      options.pid,
      options.msize,
      options.retryIntervalMs,
      serviceUnit,
      serviceUnitLen,
      hostFirewallAdmission,
      hostFirewallAdmissionLen,
      namespaceMountPaths,
      namespaceMountPathsLen,
    );
    if (status !== 0) {
      throw new Error(
        `front maintain_r9p_export(${options.srvName}) failed with status ${status}: ${this.lastError()}`,
      );
    }
  }

  reconcileR9pExports(): void {
    const status = this.library.symbols.r9p_front_reconcile_r9p_exports(this.handle);
    if (status !== 0) {
      throw new Error(
        `front reconcile_r9p_exports failed with status ${status}: ${this.lastError()}`,
      );
    }
  }

  clientRpc(options: ClientRpcOptions): string {
    const [endpoint, endpointLen] = bytes(options.endpointBind);
    const [uname, unameLen] = bytes(options.uname);
    const [aname, anameLen] = bytes(options.aname);
    const [path, pathLen] = bytes(options.path);
    const [request, requestLen] = bytes(options.request);
    const response = new Uint8Array(new ArrayBuffer(options.responseCapacity ?? 65_536));
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
    const responseLen = Number(new DataView(responseLenOut.buffer).getBigUint64(0, true));
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

  set(path: string, record: unknown): void {
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
      throw new Error(`front append_event(${path}) failed with status ${status}`);
    }
  }

  registerIntake(prefix: string): void {
    const [prefixBytes, prefixLen] = bytes(prefix);
    const status = this.library.symbols.r9p_front_register_intake(
      this.handle,
      prefixBytes,
      prefixLen,
    );
    if (status !== 0) {
      throw new Error(`front register_intake(${prefix}) failed with status ${status}`);
    }
  }

  registerRpc(path: string): void {
    const [pathBytes, pathLen] = bytes(path);
    const status = this.library.symbols.r9p_front_register_rpc(
      this.handle,
      pathBytes,
      pathLen,
    );
    if (status !== 0) {
      throw new Error(`front register_rpc(${path}) failed with status ${status}`);
    }
  }

  registerLog(path: string): void {
    const [pathBytes, pathLen] = bytes(path);
    const status = this.library.symbols.r9p_front_register_log(
      this.handle,
      pathBytes,
      pathLen,
    );
    if (status !== 0) {
      throw new Error(`front register_log(${path}) failed with status ${status}`);
    }
  }

  async nextRequest(timeoutMs: number): Promise<IntakeRequest | null> {
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
      throw new Error(`front request_prefix_copy returned ${prefixCopied}, expected ${prefixLen}`);
    }
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
    return { requestId, prefix: decoder.decode(prefixBuf), bytes: buf };
  }

  completeRequest(prefix: string, requestId: bigint, result: string): void {
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
      throw new Error(`front complete_request(${prefix}) failed with status ${status}`);
    }
  }

  close(): void {
    this.library.symbols.r9p_front_stop(this.handle);
    this.library.symbols.r9p_front_free(this.handle);
    this.library.close();
  }

  private lastError(): string {
    const empty = new Uint8Array(new ArrayBuffer(0));
    const len = Number(this.library.symbols.r9p_front_last_error(this.handle, empty, 0n));
    if (len <= 0) {
      return "no ABI error detail";
    }
    const buf = new Uint8Array(new ArrayBuffer(len));
    const copied = Number(
      this.library.symbols.r9p_front_last_error(this.handle, buf, BigInt(buf.length)),
    );
    if (copied < 0) {
      return `last_error failed with status ${copied}`;
    }
    return decoder.decode(buf.slice(0, copied));
  }
}

export function resolveFrontLibrary(flagValue: string | undefined): string | { error: string } {
  const fromEnv = Deno.env.get("R9P_FRONT_LIB");
  const path = flagValue ?? fromEnv;
  if (path === undefined || path === "") {
    return {
      error: "front library path required: pass --front-lib <path> or set R9P_FRONT_LIB",
    };
  }
  return path;
}
