import gleam/bit_array
import gleam/option.{None, Some}
import gleeunit
import gleeunit/should
import r9p
import r9p/codec
import r9p/stat

pub fn main() {
  gleeunit.main()
}

pub fn target_defaults_to_standard_msize_test() {
  let target = r9p.target("tcp!127.0.0.1!9564", "codex", "/")

  target.msize
  |> should.equal(65_536)
  target.auth_config
  |> should.equal(None)
}

pub fn target_can_select_session_auth_test() {
  let target =
    r9p.target("tcp!192.0.2.1!9564", "codex", "/")
    |> r9p.with_auth_config("/etc/r9p/client.conf")

  target.auth_config
  |> should.equal(Some("/etc/r9p/client.conf"))
}

pub fn hex_codec_roundtrips_bits_test() {
  let encoded = codec.encode_hex(<<"hello":utf8>>)

  encoded
  |> should.equal("68656c6c6f")
  let assert Ok(decoded) = codec.decode_hex(encoded)
  bit_array.to_string(decoded)
  |> should.equal(Ok("hello"))
}

pub fn stat_line_parser_decodes_machine_stat_test() {
  let assert Ok(parsed) =
    stat.parse_line(
      "stat",
      "stat\t737461747573\t0\t4\t99\t12\t420\t1\t2\t0\t0\t636f646578\t7573657273\t",
    )

  parsed.name
  |> should.equal("status")
  parsed.qid.path
  |> should.equal(99)
  parsed.uid
  |> should.equal("codex")
  parsed.muid
  |> should.equal("")
}
