import gleam/int
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string

pub type Descriptor {
  Descriptor(
    endpoint_bind: String,
    aname: String,
    uname: String,
    exported_root: String,
    transport_class: String,
    mode: String,
    auth: String,
    pid: Int,
    protocol: String,
    msize: Int,
    expires_at: Option(String),
    local_root_label: Option(String),
    namespace_mount_paths: List(String),
    extra_fields: List(#(String, String)),
  )
}

const max_u32: Int = 4_294_967_295

pub fn render(descriptor: Descriptor) -> Result(String, String) {
  use _ <- result.try(validate(descriptor))
  let fields = [
    #("format", "r9p-export.v1"),
    #("endpoint_bind", descriptor.endpoint_bind),
    #("aname", descriptor.aname),
    #("uname", descriptor.uname),
    #("exported_root", descriptor.exported_root),
    #("transport_class", descriptor.transport_class),
    #("mode", descriptor.mode),
    #("auth", descriptor.auth),
    #("pid", int.to_string(descriptor.pid)),
    #("protocol", descriptor.protocol),
    #("msize", int.to_string(descriptor.msize)),
  ]
  let fields = append_optional(fields, "expires_at", descriptor.expires_at)
  let fields =
    append_optional(fields, "local_root_label", descriptor.local_root_label)
  let fields = case descriptor.namespace_mount_paths {
    [] -> fields
    paths ->
      list.append(fields, [#("namespace_mount_paths", string.join(paths, ","))])
  }
  let extra_fields =
    list.sort(descriptor.extra_fields, fn(a, b) { string.compare(a.0, b.0) })
  render_fields(list.append(fields, extra_fields), "")
}

fn validate(descriptor: Descriptor) -> Result(Nil, String) {
  use _ <- result.try(
    validate_choice("transport_class", descriptor.transport_class, [
      "tcp",
      "unix",
    ]),
  )
  use _ <- result.try(validate_choice("mode", descriptor.mode, ["ro", "rw"]))
  use _ <- result.try(
    validate_choice("protocol", descriptor.protocol, [
      "9P2000",
      "9P2000.R",
      "9P2000.L",
    ]),
  )
  use _ <- result.try(validate_u32("pid", descriptor.pid))
  use _ <- result.try(validate_u32("msize", descriptor.msize))
  use auth_class <- result.try(auth_class(descriptor.auth))
  use _ <- result.try(validate_authority(
    descriptor.transport_class,
    descriptor.endpoint_bind,
    auth_class,
  ))
  use _ <- result.try(validate_mount_paths(descriptor.namespace_mount_paths))
  validate_extra_fields(descriptor.extra_fields)
}

fn append_optional(
  fields: List(#(String, String)),
  name: String,
  value: Option(String),
) -> List(#(String, String)) {
  case value {
    None -> fields
    Some(value) -> list.append(fields, [#(name, value)])
  }
}

fn render_fields(
  fields: List(#(String, String)),
  rendered: String,
) -> Result(String, String) {
  case fields {
    [] -> Ok(rendered)
    [#(name, value), ..rest] -> {
      use _ <- result.try(validate_token(name, name))
      use _ <- result.try(validate_token(name, value))
      render_fields(rest, rendered <> name <> "\t" <> value <> "\n")
    }
  }
}

fn validate_choice(
  name: String,
  value: String,
  choices: List(String),
) -> Result(Nil, String) {
  case list.contains(choices, value) {
    True -> Ok(Nil)
    False -> Error("unknown " <> name <> " " <> value)
  }
}

fn validate_u32(name: String, value: Int) -> Result(Nil, String) {
  case value >= 0 && value <= max_u32 {
    True -> Ok(Nil)
    False -> Error("invalid " <> name <> " " <> int.to_string(value))
  }
}

fn auth_class(auth: String) -> Result(String, String) {
  case auth {
    "none" -> Ok("none")
    _ ->
      case string.split_once(auth, on: ":") {
        Ok(#(class, details)) ->
          case
            details != ""
            && list.contains(["p9any", "uds-peercred"], class)
            && { class != "p9any" || valid_p9any_details(details) }
          {
            True -> Ok(class)
            False -> Error("invalid auth boundary " <> auth)
          }
        _ -> Error("invalid auth boundary " <> auth)
      }
  }
}

fn valid_p9any_details(details: String) -> Bool {
  case string.split_once(details, on: "@") {
    Ok(#("noise-xx", domain)) -> {
      let characters = string.to_graphemes(domain)
      characters != []
      && list.length(characters) <= 255
      && list.all(characters, fn(character) {
        string.contains(
          "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-_",
          character,
        )
      })
    }
    _ -> False
  }
}

fn validate_authority(
  transport: String,
  endpoint: String,
  auth_class: String,
) -> Result(Nil, String) {
  case transport, auth_class {
    "tcp", "none" ->
      case is_loopback(endpoint) {
        True -> Ok(Nil)
        False -> Error("descriptor auth=none is only admitted for loopback TCP")
      }
    "tcp", "uds-peercred" ->
      Error("descriptor uds-peercred auth is not valid for TCP")
    "unix", "p9any" ->
      Error("descriptor p9any session auth is not valid for unix sockets")
    _, _ -> Ok(Nil)
  }
}

fn validate_mount_paths(paths: List(String)) -> Result(Nil, String) {
  case paths {
    [] -> Ok(Nil)
    [path, ..rest] ->
      case string.starts_with(path, "/") && path != "/" {
        True -> validate_mount_paths(rest)
        False ->
          Error(
            "namespace_mount_paths entry must be absolute and non-root: "
            <> path,
          )
      }
  }
}

fn validate_extra_fields(
  fields: List(#(String, String)),
) -> Result(Nil, String) {
  case fields {
    [] -> Ok(Nil)
    [#(name, _), ..rest] ->
      case
        valid_extension_name(name)
        && !reserved(name)
        && !list.any(rest, fn(field) { field.0 == name })
      {
        True -> validate_extra_fields(rest)
        False -> Error("invalid descriptor extension field " <> name)
      }
  }
}

fn valid_extension_name(name: String) -> Bool {
  case string.to_graphemes(name) {
    [first, ..rest] ->
      string.contains("abcdefghijklmnopqrstuvwxyz", first)
      && list.all(rest, fn(char) {
        string.contains("abcdefghijklmnopqrstuvwxyz0123456789_", char)
      })
    [] -> False
  }
}

fn reserved(name: String) -> Bool {
  list.contains(
    [
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
    ],
    name,
  )
}

fn validate_token(name: String, value: String) -> Result(Nil, String) {
  case
    string.contains(value, "\t")
    || string.contains(value, "\n")
    || string.contains(value, "\r")
  {
    True -> Error("descriptor field " <> name <> " contains tab or newline")
    False -> Ok(Nil)
  }
}

fn is_loopback(endpoint: String) -> Bool {
  string.starts_with(endpoint, "127.")
  || string.starts_with(endpoint, "localhost:")
  || string.starts_with(endpoint, "[::1]:")
}
