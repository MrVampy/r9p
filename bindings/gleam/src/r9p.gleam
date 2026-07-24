import gleam/bit_array
import gleam/int
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string
import r9p/codec
import r9p/stat as r9p_stat

pub const default_msize: Int = 65_536

pub const default_timeout_ms: Int = 5000

pub const default_executable: String = "r9p-beam-port"

pub type Adapter {
  Adapter(executable: String, timeout_ms: Int)
}

pub type Target {
  Target(
    bind: String,
    uname: String,
    aname: String,
    msize: Int,
    auth_config: Option(String),
  )
}

pub type AuthorityBinding {
  AuthorityBinding(boundary: String, config_path: String)
}

pub type Resolver {
  Resolver(
    target: Target,
    service_msize: Int,
    authorities: List(AuthorityBinding),
  )
}

pub type VersionInfo {
  VersionInfo(version: String, msize: Int)
}

pub type CreateInfo {
  CreateInfo(qid: r9p_stat.Qid, iounit: Int)
}

pub fn adapter(executable: String) -> Adapter {
  Adapter(executable:, timeout_ms: default_timeout_ms)
}

pub fn default_adapter() -> Adapter {
  adapter(default_executable)
}

pub fn with_timeout(adapter: Adapter, timeout_ms: Int) -> Adapter {
  Adapter(..adapter, timeout_ms:)
}

pub fn target(bind: String, uname: String, aname: String) -> Target {
  Target(bind:, uname:, aname:, msize: default_msize, auth_config: None)
}

pub fn target_with_msize(
  bind: String,
  uname: String,
  aname: String,
  msize: Int,
) -> Target {
  Target(bind:, uname:, aname:, msize:, auth_config: None)
}

pub fn with_auth_config(target: Target, path: String) -> Target {
  Target(..target, auth_config: Some(path))
}

pub fn resolver(target: Target) -> Resolver {
  Resolver(target:, service_msize: default_msize, authorities: [])
}

pub fn resolver_with_service_msize(
  resolver: Resolver,
  service_msize: Int,
) -> Resolver {
  Resolver(..resolver, service_msize:)
}

pub fn resolver_bind_authority(
  resolver: Resolver,
  boundary: String,
  config_path: String,
) -> Resolver {
  Resolver(..resolver, authorities: [
    AuthorityBinding(boundary:, config_path:),
    ..resolver.authorities
  ])
}

pub fn resolved_stat(
  adapter: Adapter,
  resolver: Resolver,
  path: String,
) -> Result(r9p_stat.Stat, String) {
  use line <- result.try(
    run_resolved(adapter, resolver, "resolved-stat", [text(path)]),
  )
  r9p_stat.parse_line("stat", line)
}

pub fn resolved_list(
  adapter: Adapter,
  resolver: Resolver,
  path: String,
) -> Result(List(r9p_stat.Stat), String) {
  use body <- result.try(
    run_resolved(adapter, resolver, "resolved-list", [text(path)]),
  )
  body
  |> codec.lines
  |> r9p_stat.parse_lines
}

pub fn resolved_read(
  adapter: Adapter,
  resolver: Resolver,
  path: String,
) -> Result(BitArray, String) {
  use line <- result.try(
    run_resolved(adapter, resolver, "resolved-read", [text(path)]),
  )
  parse_read_line(line)
}

pub fn resolved_read_text(
  adapter: Adapter,
  resolver: Resolver,
  path: String,
) -> Result(String, String) {
  use bytes <- result.try(resolved_read(adapter, resolver, path))
  bit_array.to_string(bytes)
  |> result.map_error(fn(_) { "r9p_beam_resolved_read_non_utf8:" <> path })
}

pub fn resolved_rpc(
  adapter: Adapter,
  resolver: Resolver,
  path: String,
  data: BitArray,
) -> Result(BitArray, String) {
  use line <- result.try(
    run_resolved(adapter, resolver, "resolved-rpc", [
      text(path),
      codec.encode_hex(data),
    ]),
  )
  parse_rpc_line(line)
}

pub fn resolved_rpc_text(
  adapter: Adapter,
  resolver: Resolver,
  path: String,
  data: String,
) -> Result(String, String) {
  use bytes <- result.try(resolved_rpc(adapter, resolver, path, <<data:utf8>>))
  bit_array.to_string(bytes)
  |> result.map_error(fn(_) { "r9p_beam_resolved_rpc_non_utf8:" <> path })
}

pub fn version(adapter: Adapter, target: Target) -> Result(VersionInfo, String) {
  use line <- result.try(run(adapter, target, "version", []))
  case codec.fields(line) {
    ["version", version_hex, raw_msize] -> {
      use version <- result.try(codec.decode_text(version_hex))
      use msize <- result.try(codec.parse_int("msize", raw_msize))
      Ok(VersionInfo(version:, msize:))
    }
    _ -> Error("r9p_beam_unexpected_version_output:" <> line)
  }
}

pub fn attach(adapter: Adapter, target: Target) -> Result(r9p_stat.Qid, String) {
  use line <- result.try(run(adapter, target, "attach", []))
  parse_qid_line("attach", line)
}

pub fn stat(
  adapter: Adapter,
  target: Target,
  path: String,
) -> Result(r9p_stat.Stat, String) {
  use line <- result.try(run(adapter, target, "stat", [text(path)]))
  r9p_stat.parse_line("stat", line)
}

pub fn list(
  adapter: Adapter,
  target: Target,
  path: String,
) -> Result(List(r9p_stat.Stat), String) {
  use body <- result.try(run(adapter, target, "list", [text(path)]))
  body
  |> codec.lines
  |> r9p_stat.parse_lines
}

pub fn read(
  adapter: Adapter,
  target: Target,
  path: String,
) -> Result(BitArray, String) {
  use line <- result.try(run(adapter, target, "read", [text(path)]))
  parse_read_line(line)
}

pub fn read_text(
  adapter: Adapter,
  target: Target,
  path: String,
) -> Result(String, String) {
  use bytes <- result.try(read(adapter, target, path))
  bit_array.to_string(bytes)
  |> result.map_error(fn(_) { "r9p_beam_read_non_utf8:" <> path })
}

pub fn read_range(
  adapter: Adapter,
  target: Target,
  path: String,
  offset: Int,
  count: Int,
) -> Result(BitArray, String) {
  use line <- result.try(
    run(adapter, target, "read-range", [
      text(path),
      int.to_string(offset),
      int.to_string(count),
    ]),
  )
  parse_read_line(line)
}

pub fn write(
  adapter: Adapter,
  target: Target,
  path: String,
  offset: Int,
  data: BitArray,
) -> Result(Int, String) {
  use line <- result.try(
    run(adapter, target, "write", [
      text(path),
      int.to_string(offset),
      codec.encode_hex(data),
    ]),
  )
  parse_count_line("write", line)
}

pub fn write_file(
  adapter: Adapter,
  target: Target,
  path: String,
  data: BitArray,
) -> Result(Int, String) {
  use line <- result.try(
    run(adapter, target, "write-file", [
      text(path),
      codec.encode_hex(data),
    ]),
  )
  parse_count_line("write-file", line)
}

pub fn rpc(
  adapter: Adapter,
  target: Target,
  path: String,
  data: BitArray,
) -> Result(BitArray, String) {
  use line <- result.try(
    run(adapter, target, "rpc", [
      text(path),
      codec.encode_hex(data),
    ]),
  )
  parse_rpc_line(line)
}

pub fn rpc_text(
  adapter: Adapter,
  target: Target,
  path: String,
  data: String,
) -> Result(String, String) {
  use bytes <- result.try(rpc(adapter, target, path, <<data:utf8>>))
  bit_array.to_string(bytes)
  |> result.map_error(fn(_) { "r9p_beam_rpc_non_utf8:" <> path })
}

pub fn create(
  adapter: Adapter,
  target: Target,
  path: String,
  perm: Int,
  mode: Int,
) -> Result(CreateInfo, String) {
  use line <- result.try(
    run(adapter, target, "create", [
      text(path),
      int.to_string(perm),
      int.to_string(mode),
    ]),
  )
  parse_create_line(line)
}

pub fn create_at(
  adapter: Adapter,
  target: Target,
  parent: String,
  name: String,
  perm: Int,
  mode: Int,
) -> Result(CreateInfo, String) {
  use line <- result.try(
    run(adapter, target, "create-at", [
      text(parent),
      text(name),
      int.to_string(perm),
      int.to_string(mode),
    ]),
  )
  parse_create_line(line)
}

pub fn create_write_at(
  adapter: Adapter,
  target: Target,
  parent: String,
  name: String,
  perm: Int,
  mode: Int,
  offset: Int,
  data: BitArray,
) -> Result(Int, String) {
  use line <- result.try(
    run(adapter, target, "create-write-at", [
      text(parent),
      text(name),
      int.to_string(perm),
      int.to_string(mode),
      int.to_string(offset),
      codec.encode_hex(data),
    ]),
  )
  parse_count_line("create-write-at", line)
}

fn parse_create_line(line: String) -> Result(CreateInfo, String) {
  case codec.fields(line) {
    ["create", raw_qtype, raw_version, raw_path, raw_iounit] -> {
      use qtype <- result.try(codec.parse_int("qid_qtype", raw_qtype))
      use version <- result.try(codec.parse_int("qid_version", raw_version))
      use path <- result.try(codec.parse_int("qid_path", raw_path))
      use iounit <- result.try(codec.parse_int("iounit", raw_iounit))
      Ok(CreateInfo(
        qid: r9p_stat.Qid(qtype: qtype, version: version, path: path),
        iounit: iounit,
      ))
    }
    _ -> Error("r9p_beam_unexpected_create_output:" <> line)
  }
}

pub fn remove(
  adapter: Adapter,
  target: Target,
  path: String,
) -> Result(Nil, String) {
  use line <- result.try(run(adapter, target, "remove", [text(path)]))
  case codec.fields(line) {
    ["remove"] -> Ok(Nil)
    _ -> Error("r9p_beam_unexpected_remove_output:" <> line)
  }
}

fn run(
  adapter: Adapter,
  target: Target,
  operation: String,
  fields: List(String),
) -> Result(String, String) {
  request_port(
    adapter.executable,
    string.join(list.append([operation, ..target_fields(target)], fields), "\t"),
    adapter.timeout_ms,
  )
}

fn run_resolved(
  adapter: Adapter,
  resolver: Resolver,
  operation: String,
  fields: List(String),
) -> Result(String, String) {
  let authority_fields =
    resolver.authorities
    |> list.reverse
    |> list.flat_map(fn(binding) {
      [text(binding.boundary), text(binding.config_path)]
    })
  let header =
    list.append(
      [operation],
      list.append(target_fields(resolver.target), [
        int.to_string(resolver.service_msize),
        int.to_string(list.length(resolver.authorities)),
      ]),
    )
  request_port(
    adapter.executable,
    string.join(
      list.append(header, list.append(authority_fields, fields)),
      "\t",
    ),
    adapter.timeout_ms,
  )
}

fn target_fields(target: Target) -> List(String) {
  [
    text(target.bind),
    text(target.uname),
    text(target.aname),
    int.to_string(target.msize),
    optional_text(target.auth_config),
  ]
}

fn optional_text(value: Option(String)) -> String {
  case value {
    Some(value) -> text(value)
    None -> ""
  }
}

fn parse_read_line(line: String) -> Result(BitArray, String) {
  case codec.fields(line) {
    ["read", payload_hex] -> codec.decode_hex(payload_hex)
    ["read"] -> codec.decode_hex("")
    _ -> Error("r9p_beam_unexpected_read_output:" <> line)
  }
}

fn parse_count_line(prefix: String, line: String) -> Result(Int, String) {
  case codec.fields(line) {
    [actual, raw_count] if actual == prefix ->
      codec.parse_int(prefix <> "_count", raw_count)
    _ -> Error("r9p_beam_unexpected_" <> prefix <> "_output:" <> line)
  }
}

fn parse_rpc_line(line: String) -> Result(BitArray, String) {
  case codec.fields(line) {
    ["rpc", raw_count, payload_hex] -> {
      use count <- result.try(codec.parse_int("rpc_count", raw_count))
      use response <- result.try(codec.decode_hex(payload_hex))
      case count == bit_array.byte_size(response) {
        True -> Ok(response)
        False ->
          Error(
            "r9p_beam_rpc_count_mismatch:"
            <> int.to_string(count)
            <> ":"
            <> int.to_string(bit_array.byte_size(response)),
          )
      }
    }
    ["rpc", raw_count] -> {
      use count <- result.try(codec.parse_int("rpc_count", raw_count))
      case count == 0 {
        True -> Ok(<<>>)
        False ->
          Error("r9p_beam_rpc_count_mismatch:" <> int.to_string(count) <> ":0")
      }
    }
    _ -> Error("r9p_beam_unexpected_rpc_output:" <> line)
  }
}

fn parse_qid_line(prefix: String, line: String) -> Result(r9p_stat.Qid, String) {
  case codec.fields(line) {
    [actual, raw_qtype, raw_version, raw_path] if actual == prefix -> {
      use qtype <- result.try(codec.parse_int("qid_qtype", raw_qtype))
      use version <- result.try(codec.parse_int("qid_version", raw_version))
      use path <- result.try(codec.parse_int("qid_path", raw_path))
      Ok(r9p_stat.Qid(qtype: qtype, version: version, path: path))
    }
    _ -> Error("r9p_beam_unexpected_" <> prefix <> "_output:" <> line)
  }
}

fn text(value: String) -> String {
  codec.encode_hex(<<value:utf8>>)
}

@external(erlang, "r9p_beam_port_ffi", "request")
fn request_port(
  executable: String,
  line: String,
  timeout_ms: Int,
) -> Result(String, String)
