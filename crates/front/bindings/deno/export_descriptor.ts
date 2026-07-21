export interface ExportDescriptorOptions {
  endpointBind: string;
  aname: string;
  uname: string;
  exportedRoot: string;
  transportClass: "tcp" | "unix";
  mode: "ro" | "rw";
  auth: string;
  pid: number;
  protocol: "9P2000" | "9P2000.L";
  msize: number;
  expiresAt?: string;
  localRootLabel?: string;
  namespaceMountPaths?: string[];
  extraFields?: Readonly<Record<string, string>>;
}

const RESERVED_FIELDS = new Set([
  "format",
  "endpoint_bind",
  "aname",
  "uname",
  "exported_root",
  "transport_class",
  "mode",
  "auth",
  "pid",
  "protocol",
  "msize",
  "expires_at",
  "local_root_label",
  "namespace_mount_paths",
]);

export function renderExportDescriptor(
  options: ExportDescriptorOptions,
): string {
  validateOptions(options);
  const fields: Array<[string, string]> = [
    ["format", "r9p-export.v1"],
    ["endpoint_bind", options.endpointBind],
    ["aname", options.aname],
    ["uname", options.uname],
    ["exported_root", options.exportedRoot],
    ["transport_class", options.transportClass],
    ["mode", options.mode],
    ["auth", options.auth],
    ["pid", String(options.pid)],
    ["protocol", options.protocol],
    ["msize", String(options.msize)],
  ];
  if (options.expiresAt !== undefined) {
    fields.push(["expires_at", options.expiresAt]);
  }
  if (options.localRootLabel !== undefined) {
    fields.push(["local_root_label", options.localRootLabel]);
  }
  if (
    options.namespaceMountPaths !== undefined &&
    options.namespaceMountPaths.length > 0
  ) {
    fields.push([
      "namespace_mount_paths",
      options.namespaceMountPaths.join(","),
    ]);
  }
  for (
    const [name, value] of Object.entries(options.extraFields ?? {}).sort((
      [a],
      [b],
    ) => a.localeCompare(b))
  ) {
    validateExtensionName(name);
    if (RESERVED_FIELDS.has(name)) {
      throw new Error(`descriptor extension field ${name} is reserved`);
    }
    fields.push([name, value]);
  }
  return fields.map(([name, value]) => {
    validateToken(name, name);
    validateToken(name, value);
    return `${name}\t${value}\n`;
  }).join("");
}

function validateOptions(options: ExportDescriptorOptions): void {
  if (
    !Number.isInteger(options.pid) || options.pid < 0 ||
    options.pid > 0xffff_ffff
  ) {
    throw new Error(`invalid pid ${options.pid}`);
  }
  if (
    !Number.isInteger(options.msize) || options.msize < 0 ||
    options.msize > 0xffff_ffff
  ) {
    throw new Error(`invalid msize ${options.msize}`);
  }
  for (const path of options.namespaceMountPaths ?? []) {
    if (!path.startsWith("/") || path === "/") {
      throw new Error(
        `namespace_mount_paths entry must be absolute and non-root: ${path}`,
      );
    }
  }
  const authClass = options.auth === "none"
    ? "none"
    : options.auth.split(":", 1)[0] ?? "";
  const authDetails = options.auth === "none"
    ? ""
    : options.auth.slice(authClass.length + 1);
  if (
    !["none", "p9any", "uds-peercred"].includes(authClass) ||
    (authClass !== "none" && authDetails === "")
  ) {
    throw new Error(`invalid auth boundary ${options.auth}`);
  }
  if (
    options.transportClass === "tcp" && authClass === "none" &&
    !isLoopback(options.endpointBind)
  ) {
    throw new Error("descriptor auth=none is only admitted for loopback TCP");
  }
  if (options.transportClass === "tcp" && authClass === "uds-peercred") {
    throw new Error("descriptor uds-peercred auth is not valid for TCP");
  }
  if (
    options.transportClass === "unix" &&
    authClass === "p9any"
  ) {
    throw new Error(
      "descriptor p9any session auth is not valid for unix sockets",
    );
  }
}

function validateToken(field: string, value: string): void {
  if (value.includes("\t") || value.includes("\n") || value.includes("\r")) {
    throw new Error(`descriptor field ${field} contains tab or newline`);
  }
}

function validateExtensionName(name: string): void {
  if (!/^[a-z][a-z0-9_]*$/.test(name)) {
    throw new Error(
      `descriptor extension field ${name} must start with lowercase ascii and use lowercase ascii, digits, or underscore`,
    );
  }
}

function isLoopback(endpoint: string): boolean {
  return endpoint.startsWith("127.") || endpoint.startsWith("localhost:") ||
    endpoint.startsWith("[::1]:");
}
