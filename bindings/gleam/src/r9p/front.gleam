import gleam/int
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string
import r9p.{type Hand}
import r9p/codec

pub type Front {
  Front(hand: Hand, id: Int)
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
    open_mode: Int,
    pushed_generation: Int,
  )
}

pub type ExportPublication {
  ExportPublication(
    vault_endpoint_bind: String,
    vault_uname: String,
    vault_aname: String,
    service_name: String,
    export_endpoint_bind: String,
    export_uname: String,
    export_aname: String,
    exported_root: String,
    transport_class: String,
    auth: String,
    protocol: String,
    local_root_label: Option(String),
    msize: Int,
    retry_interval_ms: Int,
    service_unit: Option(String),
    host_firewall_admission: Option(String),
    namespace_mount_paths: List(String),
  )
}

pub type MaintenanceStatus {
  MaintenanceStatus(
    success_count: Int,
    failure_count: Int,
    last_success: Option(String),
    last_error: Option(String),
  )
}

pub fn new(hand: Hand) -> Result(Front, String) {
  use line <- result.try(run(hand, "front-new", []))
  case codec.fields(line) {
    ["front", id] -> {
      use id <- result.try(codec.parse_int("front_id", id))
      Ok(Front(hand:, id:))
    }
    _ -> Error("r9p_front_unexpected_new_output:" <> line)
  }
}

pub fn set(front: Front, path: String, data: BitArray) -> Result(Nil, String) {
  use line <- result.try(
    run(front.hand, "front-set", [
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

pub fn register_rpc(front: Front, path: String) -> Result(Nil, String) {
  use line <- result.try(
    run(front.hand, "front-register-rpc", [int.to_string(front.id), text(path)]),
  )
  expect_line("front-register-rpc", line)
}

pub fn serve_tcp(front: Front, bind: String) -> Result(String, String) {
  use line <- result.try(
    run(front.hand, "front-serve-tcp", [int.to_string(front.id), text(bind)]),
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
    run(front.hand, "front-next-request", [
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
    run(front.hand, "front-complete-request", [
      int.to_string(front.id),
      text(prefix),
      int.to_string(request_id),
      codec.encode_hex(data),
    ]),
  )
  expect_line("front-complete-request", line)
}

pub fn maintain_r9p_export(
  front: Front,
  publication: ExportPublication,
) -> Result(Nil, String) {
  use line <- result.try(
    run(front.hand, "front-maintain-r9p-export", [
      int.to_string(front.id),
      text(publication.vault_endpoint_bind),
      text(publication.vault_uname),
      text(publication.vault_aname),
      text(publication.service_name),
      text(publication.export_endpoint_bind),
      text(publication.export_uname),
      text(publication.export_aname),
      text(publication.exported_root),
      text(publication.transport_class),
      text(publication.auth),
      text(publication.protocol),
      text(optional_text(publication.local_root_label)),
      int.to_string(publication.msize),
      int.to_string(publication.retry_interval_ms),
      text(optional_text(publication.service_unit)),
      text(optional_text(publication.host_firewall_admission)),
      text(string.join(publication.namespace_mount_paths, ",")),
    ]),
  )
  expect_line("front-maintain-r9p-export", line)
}

pub fn reconcile_r9p_exports(front: Front) -> Result(Nil, String) {
  use line <- result.try(
    run(front.hand, "front-reconcile-r9p-exports", [int.to_string(front.id)]),
  )
  expect_line("front-reconcile-r9p-exports", line)
}

pub fn maintenance_status(front: Front) -> Result(MaintenanceStatus, String) {
  use line <- result.try(
    run(front.hand, "front-maintenance-status", [int.to_string(front.id)]),
  )
  case codec.fields(line) {
    [
      "front-maintenance-status",
      success_count,
      failure_count,
      last_success,
      last_error,
    ] -> {
      use success_count <- result.try(codec.parse_int(
        "success_count",
        success_count,
      ))
      use failure_count <- result.try(codec.parse_int(
        "failure_count",
        failure_count,
      ))
      use last_success <- result.try(optional_decoded_text(last_success))
      use last_error <- result.try(optional_decoded_text(last_error))
      Ok(MaintenanceStatus(
        success_count:,
        failure_count:,
        last_success:,
        last_error:,
      ))
    }
    _ -> Error("r9p_front_unexpected_maintenance_status_output:" <> line)
  }
}

pub fn stop(front: Front) -> Result(Nil, String) {
  use line <- result.try(
    run(front.hand, "front-stop", [int.to_string(front.id)]),
  )
  expect_line("front-stop", line)
}

fn run(
  hand: Hand,
  operation: String,
  fields: List(String),
) -> Result(String, String) {
  request_port(
    hand.executable,
    string.join([operation, ..fields], "\t"),
    hand.timeout_ms,
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

fn optional_text(value: Option(String)) -> String {
  case value {
    Some(inner) -> inner
    None -> ""
  }
}

fn optional_decoded_text(value: String) -> Result(Option(String), String) {
  use decoded <- result.try(codec.decode_text(value))
  case decoded {
    "" -> Ok(None)
    text -> Ok(Some(text))
  }
}

@external(erlang, "r9p_beam_port_ffi", "request")
fn request_port(
  executable: String,
  line: String,
  timeout_ms: Int,
) -> Result(String, String)
