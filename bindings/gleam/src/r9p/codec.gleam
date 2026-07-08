import gleam/bit_array
import gleam/int
import gleam/result
import gleam/string

pub fn fields(line: String) -> List(String) {
  string.split(strip_line_end(line), on: "\t")
}

pub fn lines(output: String) -> List(String) {
  case string.trim(output) {
    "" -> []
    trimmed -> string.split(trimmed, on: "\n")
  }
}

pub fn parse_int(field: String, raw: String) -> Result(Int, String) {
  int.parse(raw)
  |> result.map_error(fn(_) { "r9p_beam_invalid_" <> field <> ":" <> raw })
}

pub fn decode_text(source: String) -> Result(String, String) {
  use bytes <- result.try(decode_hex(source))
  bit_array.to_string(bytes)
  |> result.map_error(fn(_) { "r9p_beam_output_non_utf8_field" })
}

fn strip_line_end(line: String) -> String {
  case string.ends_with(line, "\n") {
    True -> strip_line_end(string.drop_end(line, 1))
    False ->
      case string.ends_with(line, "\r") {
        True -> strip_line_end(string.drop_end(line, 1))
        False -> line
      }
  }
}

@external(erlang, "r9p_beam_port_ffi", "encode_hex")
pub fn encode_hex(value: BitArray) -> String

@external(erlang, "r9p_beam_port_ffi", "decode_hex")
pub fn decode_hex(source: String) -> Result(BitArray, String)
