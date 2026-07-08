import gleam/bit_array
import gleam/int
import gleam/list
import gleam/result
import gleam/string
import r9p/codec
import r9p/stat.{type Stat}

pub const default_msize: Int = 65_536

pub const default_timeout_ms: Int = 5000

pub const default_executable: String = "r9p-beam-port"

pub type Hand {
  Hand(executable: String, timeout_ms: Int)
}

pub type Target {
  Target(bind: String, uname: String, aname: String, msize: Int)
}

pub fn hand(executable: String) -> Hand {
  Hand(executable:, timeout_ms: default_timeout_ms)
}

pub fn default_hand() -> Hand {
  hand(default_executable)
}

pub fn with_timeout(hand: Hand, timeout_ms: Int) -> Hand {
  Hand(..hand, timeout_ms:)
}

pub fn target(bind: String, uname: String, aname: String) -> Target {
  Target(bind:, uname:, aname:, msize: default_msize)
}

pub fn target_with_msize(
  bind: String,
  uname: String,
  aname: String,
  msize: Int,
) -> Target {
  Target(bind:, uname:, aname:, msize:)
}

pub fn stat(hand: Hand, target: Target, path: String) -> Result(Stat, String) {
  use line <- result.try(run(hand, target, "stat", [text(path)]))
  stat.parse_line("stat", line)
}

pub fn list(
  hand: Hand,
  target: Target,
  path: String,
) -> Result(List(Stat), String) {
  use body <- result.try(run(hand, target, "list", [text(path)]))
  body
  |> codec.lines
  |> stat.parse_lines
}

pub fn read(
  hand: Hand,
  target: Target,
  path: String,
) -> Result(BitArray, String) {
  use line <- result.try(run(hand, target, "read", [text(path)]))
  parse_read_line(line)
}

pub fn read_text(
  hand: Hand,
  target: Target,
  path: String,
) -> Result(String, String) {
  use bytes <- result.try(read(hand, target, path))
  bit_array.to_string(bytes)
  |> result.map_error(fn(_) { "r9p_beam_read_non_utf8:" <> path })
}

pub fn read_range(
  hand: Hand,
  target: Target,
  path: String,
  offset: Int,
  count: Int,
) -> Result(BitArray, String) {
  use line <- result.try(
    run(hand, target, "read-range", [
      text(path),
      int.to_string(offset),
      int.to_string(count),
    ]),
  )
  parse_read_line(line)
}

pub fn write(
  hand: Hand,
  target: Target,
  path: String,
  offset: Int,
  data: BitArray,
) -> Result(Int, String) {
  use line <- result.try(
    run(hand, target, "write", [
      text(path),
      int.to_string(offset),
      codec.encode_hex(data),
    ]),
  )
  parse_count_line("write", line)
}

pub fn rpc(
  hand: Hand,
  target: Target,
  path: String,
  data: BitArray,
) -> Result(BitArray, String) {
  use line <- result.try(
    run(hand, target, "rpc", [
      text(path),
      codec.encode_hex(data),
    ]),
  )
  parse_rpc_line(line)
}

pub fn rpc_text(
  hand: Hand,
  target: Target,
  path: String,
  data: String,
) -> Result(String, String) {
  use bytes <- result.try(rpc(hand, target, path, <<data:utf8>>))
  bit_array.to_string(bytes)
  |> result.map_error(fn(_) { "r9p_beam_rpc_non_utf8:" <> path })
}

fn run(
  hand: Hand,
  target: Target,
  operation: String,
  fields: List(String),
) -> Result(String, String) {
  request_port(
    hand.executable,
    string.join(
      list.append(
        [
          operation,
          text(target.bind),
          text(target.uname),
          text(target.aname),
          int.to_string(target.msize),
        ],
        fields,
      ),
      "\t",
    ),
    hand.timeout_ms,
  )
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

fn text(value: String) -> String {
  codec.encode_hex(<<value:utf8>>)
}

@external(erlang, "r9p_beam_port_ffi", "request")
fn request_port(
  executable: String,
  line: String,
  timeout_ms: Int,
) -> Result(String, String)
