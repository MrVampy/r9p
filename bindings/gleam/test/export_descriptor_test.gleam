import gleam/option.{None, Some}
import gleeunit/should
import r9p/export_descriptor

pub fn canonical_descriptor_order_test() {
  let assert Ok(rendered) = export_descriptor.render(descriptor())

  rendered
  |> should.equal(
    "format\tr9p-export.v1\n"
    <> "endpoint_bind\t192.168.0.20:4100\n"
    <> "aname\t/\n"
    <> "uname\tcodex\n"
    <> "exported_root\t/trades\n"
    <> "transport_class\ttcp\n"
    <> "mode\trw\n"
    <> "auth\tp9any:noise-ik@vault\n"
    <> "pid\t42\n"
    <> "protocol\t9P2000\n"
    <> "msize\t65536\n"
    <> "local_root_label\ttrades\n"
    <> "namespace_mount_paths\t/trades\n"
    <> "host_firewall_admission\t4100/tcp\n"
    <> "service_unit\ttrades.service\n",
  )
}

pub fn remote_tcp_requires_a_network_authority_test() {
  export_descriptor.render(
    export_descriptor.Descriptor(..descriptor(), auth: "none"),
  )
  |> should.be_error
}

fn descriptor() -> export_descriptor.Descriptor {
  export_descriptor.Descriptor(
    endpoint_bind: "192.168.0.20:4100",
    aname: "/",
    uname: "codex",
    exported_root: "/trades",
    transport_class: "tcp",
    mode: "rw",
    auth: "p9any:noise-ik@vault",
    pid: 42,
    protocol: "9P2000",
    msize: 65_536,
    expires_at: None,
    local_root_label: Some("trades"),
    namespace_mount_paths: ["/trades"],
    extra_fields: [
      #("service_unit", "trades.service"),
      #("host_firewall_admission", "4100/tcp"),
    ],
  )
}
