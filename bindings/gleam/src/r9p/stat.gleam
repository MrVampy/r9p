import gleam/list
import gleam/result
import r9p/codec

pub type Qid {
  Qid(qtype: Int, version: Int, path: Int)
}

pub type Stat {
  Stat(
    stat_type: Int,
    dev: Int,
    qid: Qid,
    mode: Int,
    atime: Int,
    mtime: Int,
    length: Int,
    name: String,
    uid: String,
    gid: String,
    muid: String,
  )
}

pub fn parse_lines(source: List(String)) -> Result(List(Stat), String) {
  parse_lines_loop(source, [])
}

pub fn parse_line(prefix: String, line: String) -> Result(Stat, String) {
  case codec.fields(line) {
    [
      actual,
      name_hex,
      raw_qtype,
      raw_version,
      raw_path,
      raw_length,
      raw_mode,
      raw_atime,
      raw_mtime,
      raw_stat_type,
      raw_dev,
      uid_hex,
      gid_hex,
      muid_hex,
    ]
      if actual == prefix
    -> {
      use name <- result.try(codec.decode_text(name_hex))
      use qtype <- result.try(codec.parse_int("qtype", raw_qtype))
      use version <- result.try(codec.parse_int("qid_version", raw_version))
      use path <- result.try(codec.parse_int("qid_path", raw_path))
      use length <- result.try(codec.parse_int("length", raw_length))
      use mode <- result.try(codec.parse_int("mode", raw_mode))
      use atime <- result.try(codec.parse_int("atime", raw_atime))
      use mtime <- result.try(codec.parse_int("mtime", raw_mtime))
      use stat_type <- result.try(codec.parse_int("stat_type", raw_stat_type))
      use dev <- result.try(codec.parse_int("dev", raw_dev))
      use uid <- result.try(codec.decode_text(uid_hex))
      use gid <- result.try(codec.decode_text(gid_hex))
      use muid <- result.try(codec.decode_text(muid_hex))
      Ok(Stat(
        stat_type: stat_type,
        dev: dev,
        qid: Qid(qtype: qtype, version: version, path: path),
        mode: mode,
        atime: atime,
        mtime: mtime,
        length: length,
        name: name,
        uid: uid,
        gid: gid,
        muid: muid,
      ))
    }
    _ -> Error("r9p_beam_unexpected_stat_output:" <> line)
  }
}

fn parse_lines_loop(
  source: List(String),
  acc: List(Stat),
) -> Result(List(Stat), String) {
  case source {
    [] -> Ok(list.reverse(acc))
    [line, ..rest] -> {
      use parsed <- result.try(parse_line("entry", line))
      parse_lines_loop(rest, [parsed, ..acc])
    }
  }
}
