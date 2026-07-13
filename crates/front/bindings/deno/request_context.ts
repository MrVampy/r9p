export interface RequestContext {
  version: "r9p-front-request-context.v1";
  principalId: string;
  uname: string;
  aname: string;
  sessionId: bigint;
  fid: bigint;
  targetPath: string;
  offset: bigint;
  openMode: number;
  pushedGeneration: bigint;
  raw: string;
}

export function parseRequestContext(raw: string): RequestContext {
  const version = lfeStringField(raw, "version");
  if (version !== "r9p-front-request-context.v1") {
    throw new Error(`unsupported front request context version: ${version}`);
  }
  const openMode = lfeNumberField(raw, "open_mode");
  if (openMode < 0n || openMode > 255n) {
    throw new Error(
      `front request context open_mode out of range: ${openMode}`,
    );
  }
  return {
    version,
    principalId: lfeStringField(raw, "principal_id"),
    uname: lfeStringField(raw, "uname"),
    aname: lfeStringField(raw, "aname"),
    sessionId: lfeNumberField(raw, "session_id"),
    fid: lfeNumberField(raw, "fid"),
    targetPath: lfeStringField(raw, "target_path"),
    offset: lfeNumberField(raw, "offset"),
    openMode: Number(openMode),
    pushedGeneration: lfeNumberField(raw, "pushed_generation"),
    raw,
  };
}

function lfeStringField(raw: string, name: string): string {
  const marker = `"${name}" "`;
  const start = raw.indexOf(marker);
  if (start < 0) {
    throw new Error(`front request context missing string field: ${name}`);
  }
  let index = start + marker.length;
  let value = "";
  while (index < raw.length) {
    const ch = raw[index];
    if (ch === '"') return value;
    if (ch !== "\\") {
      value += ch;
      index += 1;
      continue;
    }
    index += 1;
    const escaped = raw[index];
    if (escaped === undefined) {
      throw new Error(
        `front request context unterminated escape in field: ${name}`,
      );
    }
    switch (escaped) {
      case "\\":
        value += "\\";
        break;
      case '"':
        value += '"';
        break;
      case "n":
        value += "\n";
        break;
      case "r":
        value += "\r";
        break;
      case "t":
        value += "\t";
        break;
      default:
        value += escaped;
        break;
    }
    index += 1;
  }
  throw new Error(`front request context unterminated string field: ${name}`);
}

function lfeNumberField(raw: string, name: string): bigint {
  const marker = `"${name}" `;
  const start = raw.indexOf(marker);
  if (start < 0) {
    throw new Error(`front request context missing number field: ${name}`);
  }
  const rest = raw.slice(start + marker.length);
  const match = /^-?\d+/.exec(rest);
  if (match === null) {
    throw new Error(`front request context invalid number field: ${name}`);
  }
  return BigInt(match[0]);
}
