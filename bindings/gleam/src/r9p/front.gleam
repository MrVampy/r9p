import gleam/int
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string
import r9p.{type Adapter}
import r9p/codec

pub type Front {
  Front(adapter: Adapter, id: Int)
}

pub type Request {
  Request(
    prefix: String,
    request_id: Int,
    bytes: BitArray,
    context: RequestContext,
  )
}

pub type RequestContext {
  RequestContext(
    principal_id: String,
    uname: String,
    aname: String,
    session_id: Int,
    fid: Int,
    front_path: String,
    target_path: String,
    offset: Int,
    count: Int,
    open_mode: Int,
    pushed_generation: Int,
  )
}

pub fn new(adapter: Adapter) -> Result(Front, String) {
  use line <- result.try(run(adapter, "front-new", []))
  case codec.fields(line) {
    ["front", id] -> {
      use id <- result.try(codec.parse_int("front_id", id))
      Ok(Front(adapter:, id:))
    }
    _ -> Error("r9p_front_unexpected_new_output:" <> line)
  }
}

pub fn process_id(front: Front) -> Result(Int, String) {
  use line <- result.try(
    run(front.adapter, "front-process-id", [int.to_string(front.id)]),
  )
  case codec.fields(line) {
    ["front-process-id", process_id] ->
      codec.parse_int("front_process_id", process_id)
    _ -> Error("r9p_front_unexpected_process_id_output:" <> line)
  }
}

pub fn set(front: Front, path: String, data: BitArray) -> Result(Nil, String) {
  use line <- result.try(
    run(front.adapter, "front-set", [
      int.to_string(front.id),
      text(path),
      codec.encode_hex(data),
    ]),
  )
  expect_line("front-set", line)
}

pub fn set_text(front: Front, path: String, data: String) -> Result(Nil, String) {
  set(front, path, <<data:utf8>>)
}

pub fn remove_subtree(front: Front, path: String) -> Result(Nil, String) {
  use line <- result.try(
    run(front.adapter, "front-remove-subtree", [
      int.to_string(front.id),
      text(path),
    ]),
  )
  expect_line("front-remove-subtree", line)
}

pub fn register_log(front: Front, path: String) -> Result(Nil, String) {
  use line <- result.try(
    run(front.adapter, "front-register-log", [
      int.to_string(front.id),
      text(path),
    ]),
  )
  expect_line("front-register-log", line)
}

pub fn append_event(
  front: Front,
  path: String,
  data: BitArray,
) -> Result(Nil, String) {
  use line <- result.try(
    run(front.adapter, "front-append-event", [
      int.to_string(front.id),
      text(path),
      codec.encode_hex(data),
    ]),
  )
  expect_line("front-append-event", line)
}

pub fn append_event_text(
  front: Front,
  path: String,
  data: String,
) -> Result(Nil, String) {
  append_event(front, path, <<data:utf8>>)
}

pub fn register_rpc(front: Front, path: String) -> Result(Nil, String) {
  use line <- result.try(
    run(front.adapter, "front-register-rpc", [
      int.to_string(front.id),
      text(path),
    ]),
  )
  expect_line("front-register-rpc", line)
}

pub fn register_read_relay(front: Front, path: String) -> Result(Nil, String) {
  use line <- result.try(
    run(front.adapter, "front-register-read-relay", [
      int.to_string(front.id),
      text(path),
    ]),
  )
  expect_line("front-register-read-relay", line)
}

pub fn register_remove_relay(front: Front, path: String) -> Result(Nil, String) {
  use line <- result.try(
    run(front.adapter, "front-register-remove-relay", [
      int.to_string(front.id),
      text(path),
    ]),
  )
  expect_line("front-register-remove-relay", line)
}

pub fn serve_tcp(front: Front, bind: String) -> Result(String, String) {
  use line <- result.try(
    run(front.adapter, "front-serve-tcp", [int.to_string(front.id), text(bind)]),
  )
  case codec.fields(line) {
    ["front-serve-tcp", address] -> codec.decode_text(address)
    _ -> Error("r9p_front_unexpected_serve_tcp_output:" <> line)
  }
}

pub fn next_request(
  front: Front,
  timeout_ms: Int,
) -> Result(Option(Request), String) {
  use line <- result.try(
    run(front.adapter, "front-next-request", [
      int.to_string(front.id),
      int.to_string(timeout_ms),
    ]),
  )
  case codec.fields(line) {
    ["front-timeout"] -> Ok(None)
    [
      "front-request",
      prefix,
      request_id,
      bytes,
      principal_id,
      uname,
      aname,
      session_id,
      fid,
      front_path,
      target_path,
      offset,
      count,
      open_mode,
      pushed_generation,
    ] -> {
      use prefix <- result.try(codec.decode_text(prefix))
      use request_id <- result.try(codec.parse_int("request_id", request_id))
      use bytes <- result.try(codec.decode_hex(bytes))
      use principal_id <- result.try(codec.decode_text(principal_id))
      use uname <- result.try(codec.decode_text(uname))
      use aname <- result.try(codec.decode_text(aname))
      use session_id <- result.try(codec.parse_int("session_id", session_id))
      use fid <- result.try(codec.parse_int("fid", fid))
      use front_path <- result.try(codec.decode_text(front_path))
      use target_path <- result.try(codec.decode_text(target_path))
      use offset <- result.try(codec.parse_int("offset", offset))
      use count <- result.try(codec.parse_int("count", count))
      use open_mode <- result.try(codec.parse_int("open_mode", open_mode))
      use pushed_generation <- result.try(codec.parse_int(
        "pushed_generation",
        pushed_generation,
      ))
      Ok(
        Some(Request(
          prefix:,
          request_id:,
          bytes:,
          context: RequestContext(
            principal_id:,
            uname:,
            aname:,
            session_id:,
            fid:,
            front_path:,
            target_path:,
            offset:,
            count:,
            open_mode:,
            pushed_generation:,
          ),
        )),
      )
    }
    _ -> Error("r9p_front_unexpected_next_request_output:" <> line)
  }
}

pub fn complete_request(
  front: Front,
  prefix: String,
  request_id: Int,
  data: BitArray,
) -> Result(Nil, String) {
  use line <- result.try(
    run(front.adapter, "front-complete-request", [
      int.to_string(front.id),
      text(prefix),
      int.to_string(request_id),
      codec.encode_hex(data),
    ]),
  )
  expect_line("front-complete-request", line)
}

pub fn reject_request(
  front: Front,
  prefix: String,
  request_id: Int,
  message: String,
) -> Result(Nil, String) {
  use line <- result.try(
    run(front.adapter, "front-reject-request", [
      int.to_string(front.id),
      text(prefix),
      int.to_string(request_id),
      text(message),
    ]),
  )
  expect_line("front-reject-request", line)
}

pub fn complete_remove(
  front: Front,
  prefix: String,
  request_id: Int,
) -> Result(Nil, String) {
  use line <- result.try(
    run(front.adapter, "front-complete-remove", [
      int.to_string(front.id),
      text(prefix),
      int.to_string(request_id),
    ]),
  )
  expect_line("front-complete-remove", line)
}

pub fn reject_remove(
  front: Front,
  prefix: String,
  request_id: Int,
  message: String,
) -> Result(Nil, String) {
  use line <- result.try(
    run(front.adapter, "front-reject-remove", [
      int.to_string(front.id),
      text(prefix),
      int.to_string(request_id),
      text(message),
    ]),
  )
  expect_line("front-reject-remove", line)
}

pub fn stop(front: Front) -> Result(Nil, String) {
  use line <- result.try(
    run(front.adapter, "front-stop", [int.to_string(front.id)]),
  )
  expect_line("front-stop", line)
}

fn run(
  adapter: Adapter,
  operation: String,
  fields: List(String),
) -> Result(String, String) {
  request_port(
    adapter.executable,
    string.join([operation, ..fields], "\t"),
    adapter.timeout_ms,
  )
}

fn expect_line(expected: String, line: String) -> Result(Nil, String) {
  case line == expected {
    True -> Ok(Nil)
    False -> Error("r9p_front_unexpected_" <> expected <> "_output:" <> line)
  }
}

fn text(value: String) -> String {
  codec.encode_hex(<<value:utf8>>)
}

// Fronts are process-owned state. Keep them isolated from client request
// timeouts, which may restart their own port to preserve protocol framing.
@external(erlang, "r9p_beam_port_ffi", "front_request")
fn request_port(
  executable: String,
  line: String,
  timeout_ms: Int,
) -> Result(String, String)
